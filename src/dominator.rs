//! SEMI-NCA dominator-tree algorithm (Georgiadis & Tarjan).
//!
//! A single-pass O(m·α(n)) dominator algorithm — no fixed-point iteration.
//! This replaces the Cooper-Harvey-Kennedy iterative approach, which needs
//! O(chain-length) iterations on heap graphs with long dominator chains
//! (each backward-propagating fix advances only one node per pass).
//!
//! # Algorithm
//! Works in DFS **pre-order** index space (`vertex[i]` = node at pre-order `i`,
//! `dfn[v]` = pre-order number of node `v`; virtual root = pre-order 0).
//!
//! 1. Compute semidominators `semi[w]` for every vertex `w` (reverse pre-order),
//!    using an eval/link union-find with path compression over predecessors.
//! 2. Compute immediate dominators via nearest-common-ancestor in the
//!    semidominator forest (forward pre-order).
//!
//! # Indexing convention
//! Objects are 0..n-1; the virtual root is index `n`.
//! Returns `idom: Vec<u32>` of length `n+1`.
//! `idom[n] = n` (virtual root dominates itself).
//! `idom[v] = n` when the virtual root is the immediate dominator of `v`.
//! Unreachable nodes retain `UNDEFINED`.

use crate::{rpo_dfs::RpoResult, vbyte};

const UNDEFINED: u32 = u32::MAX;

/// Compute immediate dominators for all reachable nodes using SEMI-NCA.
pub fn compute_dominators(
    n: usize,
    rpo: &RpoResult,
    gc_root_indices: &[u32],
    inb_offsets: &[u32], // byte offsets into inb_data, len = n+1
    inb_data: &[u8],
) -> Vec<u32> {
    let vroot = n as u32;

    // Number of reachable vertices (including virtual root at pre-order 0).
    let count = rpo.vertex.len();

    // ── Work arrays in pre-order index space (0..count) ──────────────────
    // parent info is read directly from rpo.parent_pre (no local copy)
    // semi[i]    = pre-order number of semidominator of vertex[i]
    // ancestor/label = union-find with path compression (dead after Phase 1)
    // idom_pre[i]= pre-order number of immediate dominator of vertex[i]; not
    //              allocated separately — reuses the `ancestor` buffer after
    //              Phase 1 (see rebind below), saving one count-length u32 array.
    let mut semi = vec![0u32; count];
    let mut ancestor = vec![0u32; count]; // 0 = no ancestor (unlinked)
    let mut label = vec![0u32; count];

    // Initialize: semi[i] = i, label[i] = i, parent[i] = parent_pre[i]
    for i in 0..count {
        semi[i] = i as u32;
        label[i] = i as u32;
        ancestor[i] = 0;
    }

    // vr_adjacent: which nodes have an implicit virtual-root predecessor (GC roots).
    // For these, the virtual root is always a predecessor in the eval step.
    let mut vr_adjacent = vec![false; n + 1];
    for &r in gc_root_indices {
        vr_adjacent[r as usize] = true;
    }

    // ── Phase 1: compute semidominators (reverse pre-order, i = count-1 .. 1) ──
    for i in (1..count).rev() {
        let w_node = rpo.vertex[i] as usize;

        // For each predecessor v of w:
        //   if v is reachable, u = eval(dfn[v]); if semi[u] < semi[i], semi[i] = semi[u]
        let process_pred = |pred_node: usize,
                                semi: &mut [u32],
                                ancestor: &mut [u32],
                                label: &mut [u32]| {
            let pv = rpo.dfn[pred_node];
            if pv == UNDEFINED {
                return; // predecessor unreachable
            }
            let u = eval(pv, ancestor, label, semi);
            if semi[u as usize] < semi[i] {
                semi[i] = semi[u as usize];
            }
        };

        // Implicit virtual-root predecessor for GC roots
        if vr_adjacent[w_node] {
            // eval(0) = 0, semi[0] = 0 → semi[i] becomes 0 (dominated by vroot)
            let u = eval(0, &mut ancestor, &mut label, &semi);
            if semi[u as usize] < semi[i] {
                semi[i] = semi[u as usize];
            }
        }

        // Decode inbound predecessor list for w
        let byte_start = inb_offsets[w_node] as usize;
        let byte_end = inb_offsets[w_node + 1] as usize;
        let mut pos = byte_start;
        let mut prev: u32 = 0;
        while pos < byte_end {
            let (delta, consumed) = vbyte::decode_one(&inb_data[pos..]);
            pos += consumed;
            let pred = prev.wrapping_add(delta);
            prev = pred;
            process_pred(pred as usize, &mut semi, &mut ancestor, &mut label);
        }

        // Link w to its parent in the forest
        // ancestor[i] = parent[i]; (path-compression union-find)
        link(i as u32, rpo.parent_pre[i], &mut ancestor);
    }

    // Phase 1 done: `ancestor` and `label` are dead. Reuse `ancestor`'s
    // buffer as `idom_pre` (no new allocation) and free `label`.
    let mut idom_pre = ancestor;
    drop(label);

    // ── Phase 2: compute immediate dominators (forward pre-order) ────────
    // SEMI-NCA: idom[w] = nearest ancestor of parent[w] on the DFS path
    // whose pre-order <= semi[w].
    idom_pre[0] = 0;
    for i in 1..count {
        let mut d = rpo.parent_pre[i];
        while d > semi[i] {
            d = idom_pre[d as usize];
        }
        idom_pre[i] = d;
    }

    // ── Translate pre-order idom back to node-index space ────────────────
    let mut idom = vec![UNDEFINED; n + 1];
    idom[n] = vroot; // virtual root dominates itself
    for i in 1..count {
        let node = rpo.vertex[i] as usize;
        let dom_pre = idom_pre[i];
        let dom_node = rpo.vertex[dom_pre as usize];
        // If the dominator is the virtual root (pre-order 0), store vroot (= n)
        idom[node] = if dom_pre == 0 { vroot } else { dom_node };
    }

    idom
}

/// eval(v): find the vertex on the path v..root (in the union-find forest)
/// with the minimum semi value, applying path compression.
/// Returns the label (pre-order index) of that minimum-semi vertex.
fn eval(v: u32, ancestor: &mut [u32], label: &mut [u32], semi: &[u32]) -> u32 {
    if ancestor[v as usize] == 0 {
        return label[v as usize];
    }
    compress(v, ancestor, label, semi);
    label[v as usize]
}

/// Iterative path compression: collect the path to the root, then update
/// labels/ancestors from the top down.  No recursion (heaps have millions of nodes).
fn compress(v: u32, ancestor: &mut [u32], label: &mut [u32], semi: &[u32]) {
    // Collect chain v → ancestor[v] → ... while ancestor != 0
    let mut chain: Vec<u32> = Vec::new();
    let mut x = v;
    while ancestor[ancestor[x as usize] as usize] != 0 {
        chain.push(x);
        x = ancestor[x as usize];
    }
    // Now ancestor[ancestor[x]] == 0, i.e. ancestor[x] is a forest root.
    // Process the chain from the one closest to root downward.
    // For each node in reverse: update label to the min-semi of itself vs its ancestor's label.
    for &node in chain.iter().rev() {
        let anc = ancestor[node as usize];
        if semi[label[anc as usize] as usize] < semi[label[node as usize] as usize] {
            label[node as usize] = label[anc as usize];
        }
        ancestor[node as usize] = ancestor[anc as usize];
    }
}

/// link(w, parent): attach w to parent in the union-find forest.
fn link(w: u32, parent: u32, ancestor: &mut [u32]) {
    ancestor[w as usize] = parent;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpo_dfs::rpo_dfs;

    // Build inbound CSR (VByte delta-encoded) from a per-node predecessor list.
    fn build_inb(n: usize, preds: &[Vec<u32>]) -> (Vec<u32>, Vec<u8>) {
        let mut offsets = Vec::with_capacity(n + 2);
        let mut data = Vec::new();
        offsets.push(0u32);
        for i in 0..n {
            let mut sorted = preds.get(i).cloned().unwrap_or_default();
            sorted.sort_unstable();
            sorted.dedup();
            let mut prev = 0u32;
            for &v in &sorted {
                vbyte::encode(v - prev, &mut data);
                prev = v;
            }
            offsets.push(data.len() as u32);
        }
        // sentinel for node n (virtual root) — no preds
        offsets.push(data.len() as u32);
        (offsets, data)
    }

    #[test]
    fn diamond_idom() {
        // vroot→0, 0→{1,2}, 1→3, 2→3
        let fwd_off = vec![0u32, 2, 3, 4, 4];
        let fwd_tgt = vec![1u32, 2, 3, 3];
        let roots = vec![0u32];
        let rpo = rpo_dfs(4, &roots, &fwd_off, &fwd_tgt);
        // inbound: node1←[0], node2←[0], node3←[1,2]
        let preds = vec![vec![], vec![0u32], vec![0u32], vec![1u32, 2u32]];
        let (inb_offsets, inb_data) = build_inb(4, &preds);
        let idom = compute_dominators(4, &rpo, &roots, &inb_offsets, &inb_data);
        assert_eq!(idom[0], 4, "idom[0]=vroot");
        assert_eq!(idom[1], 0, "idom[1]=0");
        assert_eq!(idom[2], 0, "idom[2]=0");
        assert_eq!(idom[3], 0, "idom[3]=0 (both paths through 0)");
    }

    #[test]
    fn chain_idom() {
        // vroot→0→1→2
        let fwd_off = vec![0u32, 1, 2, 2];
        let fwd_tgt = vec![1u32, 2u32];
        let roots = vec![0u32];
        let rpo = rpo_dfs(3, &roots, &fwd_off, &fwd_tgt);
        let preds = vec![vec![], vec![0u32], vec![1u32]];
        let (inb_offsets, inb_data) = build_inb(3, &preds);
        let idom = compute_dominators(3, &rpo, &roots, &inb_offsets, &inb_data);
        assert_eq!(idom[0], 3, "idom[0]=vroot");
        assert_eq!(idom[1], 0);
        assert_eq!(idom[2], 1);
    }

    #[test]
    fn two_roots_no_shared_path() {
        // vroot→{0,1}
        let fwd_off = vec![0u32, 0, 0];
        let fwd_tgt = vec![];
        let roots = vec![0u32, 1u32];
        let rpo = rpo_dfs(2, &roots, &fwd_off, &fwd_tgt);
        let preds = vec![vec![], vec![]];
        let (inb_offsets, inb_data) = build_inb(2, &preds);
        let idom = compute_dominators(2, &rpo, &roots, &inb_offsets, &inb_data);
        let vroot = 2u32;
        assert_eq!(idom[0], vroot);
        assert_eq!(idom[1], vroot);
    }

    #[test]
    fn reconvergent_diamond_with_bypass() {
        // vroot→0, 0→{1,2}, 1→2, 2→3, 1→3
        // node 3 reachable from both 1 and 2; node 2 reachable from 0 and 1.
        // idom[2]=0 (0→2 direct and 0→1→2), idom[3]=0 (0→1→3 and 0→..→2→3)
        let fwd_off = vec![0u32, 2, 4, 5, 5];
        let fwd_tgt = vec![1u32, 2, 2, 3, 3];
        let roots = vec![0u32];
        let rpo = rpo_dfs(4, &roots, &fwd_off, &fwd_tgt);
        // inbound: 1←[0], 2←[0,1], 3←[1,2]
        let preds = vec![vec![], vec![0u32], vec![0u32, 1u32], vec![1u32, 2u32]];
        let (inb_offsets, inb_data) = build_inb(4, &preds);
        let idom = compute_dominators(4, &rpo, &roots, &inb_offsets, &inb_data);
        assert_eq!(idom[0], 4);
        assert_eq!(idom[1], 0);
        assert_eq!(idom[2], 0, "2 dominated by 0 (direct edge bypasses 1)");
        assert_eq!(idom[3], 0, "3 dominated by 0 (paths via 1 and via 2)");
    }
}
