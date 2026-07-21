//! Query-gated field-labeled reference-edge capture for N-hop RefWalk.
//!
//! CARRY-OUT (from Task 0): `Pass2::build` returns a 6-tuple whose 6th element
//! is `crate::query::execute::QueryExecState` (`src/pass2/mod.rs:65-72`, built at
//! `mod.rs:485` via `scan_driver.finish_state()`). `main.rs:989` binds it and
//! flows it unmodified into `resume(query_state, .., &LateCtx{..})` at
//! `main.rs:1161`. Task 5 extends that tuple to also carry the built CSR +
//! interned `field_names`, threading the borrowed slices into `LateCtx.fwd_*`.
//!
//! Only populated when an active query has `plan.needs.ref_walk`. Captures
//! `(src_dense, field_id, dst_dense)` edges for the specific hop fields the
//! queries name, capped, then folds them into a small per-field forward CSR.

/// Deduplicated hop field names across all active RefWalk queries, in first-seen
/// order. `field_id` is the index into this table.
pub fn intern_hop_fields(per_query_hops: &[Vec<String>]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for hops in per_query_hops {
        for h in hops {
            if !out.iter().any(|x| x == h) {
                out.push(h.clone());
            }
        }
    }
    out
}

/// Capped accumulator of field-labeled edges captured during the scan.
pub struct RefWalkEdges {
    edges: Vec<(u32, u32, u32)>, // (src_dense, field_id, dst_dense)
    cap: usize,
    truncated: bool,
}

impl RefWalkEdges {
    pub fn new(cap: usize) -> Self {
        Self { edges: Vec::new(), cap, truncated: false }
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn len(&self) -> usize {
        self.edges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// Record an edge; drops + marks truncated once the cap is hit.
    pub fn push(&mut self, src: u32, field_id: u32, dst: u32) {
        if self.edges.len() >= self.cap {
            self.truncated = true;
            return;
        }
        self.edges.push((src, field_id, dst));
    }

    /// Fold captured edges into a per-src CSR over `n` nodes: returns
    /// (fwd_off[len n+1], fwd_tgt, fwd_field). Edges are grouped by src via a
    /// counting sort; within a src, insertion (push) order is preserved.
    pub fn into_csr(mut self, n: usize) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
        let mut off = vec![0u32; n + 1];
        for &(s, _, _) in &self.edges {
            off[s as usize + 1] += 1;
        }
        for i in 0..n {
            off[i + 1] += off[i];
        }
        let total = self.edges.len();
        let mut tgt = vec![0u32; total];
        let mut fid = vec![0u32; total];
        let mut cursor: Vec<u32> = off[..n].to_vec();
        // Stable within src: iterate edges in push order, place at cursor[src]++.
        for (s, f, d) in self.edges.drain(..) {
            let p = cursor[s as usize] as usize;
            tgt[p] = d;
            fid[p] = f;
            cursor[s as usize] += 1;
        }
        (off, tgt, fid)
    }
}

/// Overall cap on captured RefWalk edges (mirrors the `FIELD_REF_CAP` idiom).
pub const REFWALK_EDGE_CAP: usize = 5_000_000;

/// Gather every reference-hop field name a query walks, across SELECT and WHERE
/// `Attr::RefPath` occurrences. These are the fields whose object references
/// must be captured as edges during the scan. Order is first-seen; duplicates
/// within one query are kept out here so `intern_hop_fields` can dedup across
/// queries. The `tail` of a RefPath is a projection, not a hop, so it is not
/// included (unless it is itself a nested RefPath, which the recursion covers).
pub fn refwalk_field_names(q: &crate::query::ast::Query) -> Vec<String> {
    use crate::query::ast::{Attr, Predicate, SelectItem};

    fn collect_attr(a: &Attr, out: &mut Vec<String>) {
        if let Attr::RefPath { hops, tail, .. } = a {
            for h in hops {
                if !out.iter().any(|x| x == h) {
                    out.push(h.clone());
                }
            }
            collect_attr(tail, out);
        }
    }
    fn collect_pred(p: &Predicate, out: &mut Vec<String>) {
        match p {
            Predicate::And(a, b) | Predicate::Or(a, b) => {
                collect_pred(a, out);
                collect_pred(b, out);
            }
            Predicate::Not(a) => collect_pred(a, out),
            Predicate::Compare { lhs, .. } => collect_attr(lhs, out),
            Predicate::InSubquery { .. } | Predicate::InstanceOf(_) => {}
        }
    }

    let mut out = Vec::new();
    for item in &q.select {
        match item {
            SelectItem::Attr(a) => collect_attr(a, &mut out),
            SelectItem::Aggregate { arg, .. } => {
                if let SelectItem::Attr(a) = arg.as_ref() {
                    collect_attr(a, &mut out);
                }
            }
            SelectItem::Star => {}
        }
    }
    if let Some(pred) = &q.where_ {
        collect_pred(pred, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_dedups_hop_fields_across_queries() {
        let names = intern_hop_fields(&[
            vec!["parent".into()],
            vec!["parent".into(), "next".into()],
        ]);
        assert_eq!(names, vec!["parent".to_string(), "next".to_string()]);
    }

    #[test]
    fn intern_preserves_first_seen_order() {
        let names = intern_hop_fields(&[
            vec!["b".into(), "a".into()],
            vec!["a".into(), "c".into(), "b".into()],
        ]);
        assert_eq!(names, vec!["b".to_string(), "a".to_string(), "c".to_string()]);
    }

    #[test]
    fn edges_into_csr_sorts_by_src_and_offsets() {
        let mut e = RefWalkEdges::new(100);
        e.push(2, 0, 9);
        e.push(0, 0, 5);
        e.push(0, 1, 7);
        let (off, tgt, fid) = e.into_csr(3);
        // node 0 has 2 edges, node 1 none, node 2 one
        assert_eq!(off, vec![0, 2, 2, 3]);
        assert_eq!(tgt, vec![5, 7, 9]);
        assert_eq!(fid, vec![0, 1, 0]);
    }

    #[test]
    fn edges_cap_sets_truncated() {
        let mut e = RefWalkEdges::new(1);
        e.push(0, 0, 1);
        assert!(!e.truncated());
        e.push(0, 0, 2);
        assert!(e.truncated());
        assert_eq!(e.len(), 1);
    }

    #[test]
    fn empty_edges_into_csr_all_zero_offsets() {
        let e = RefWalkEdges::new(10);
        assert!(e.is_empty());
        let (off, tgt, fid) = e.into_csr(4);
        assert_eq!(off, vec![0, 0, 0, 0, 0]);
        assert!(tgt.is_empty());
        assert!(fid.is_empty());
    }

    #[test]
    fn edge_on_last_node_boundary() {
        // Only src n-1 has an edge; offsets must not overflow the n+1 array.
        let mut e = RefWalkEdges::new(10);
        e.push(3, 0, 42);
        let (off, tgt, fid) = e.into_csr(4);
        assert_eq!(off, vec![0, 0, 0, 0, 1]);
        assert_eq!(tgt, vec![42]);
        assert_eq!(fid, vec![0]);
    }

    #[test]
    fn multiple_fields_on_one_src_preserve_dst_pairing() {
        let mut e = RefWalkEdges::new(10);
        e.push(0, 0, 100);
        e.push(0, 2, 200);
        e.push(0, 1, 300);
        let (off, tgt, fid) = e.into_csr(1);
        assert_eq!(off, vec![0, 3]);
        // push order preserved within src: (fid,dst) pairs stay aligned.
        assert_eq!(fid, vec![0, 2, 1]);
        assert_eq!(tgt, vec![100, 200, 300]);
    }

    #[test]
    fn refwalk_field_names_gathers_select_and_where_hops() {
        let q = crate::query::parse::parse(
            "SELECT x.parent.name FROM C x WHERE x.next.hash > 0",
        )
        .unwrap();
        let names = refwalk_field_names(&q);
        // hop fields only (parent, next); tails name/hash are projections, not hops.
        assert!(names.contains(&"parent".to_string()));
        assert!(names.contains(&"next".to_string()));
        assert!(!names.contains(&"name".to_string()));
        assert!(!names.contains(&"hash".to_string()));
    }

    #[test]
    fn refwalk_field_names_empty_when_no_refpath() {
        let q = crate::query::parse::parse("SELECT x.count FROM C x").unwrap();
        assert!(refwalk_field_names(&q).is_empty());
    }
}
