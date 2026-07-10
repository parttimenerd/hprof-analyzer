//! Cooper-Harvey-Kennedy (CHK) iterative dominator algorithm.
//!
//! Reference: "A Simple, Fast Dominance Algorithm" by Cooper, Harvey & Kennedy.
//!
//! # Indexing convention
//! Objects are 0..n-1; the virtual root is index `n`.

use crate::{rpo_dfs::RpoResult, vbyte};

const UNDEFINED: u32 = u32::MAX;

/// Compute immediate dominators for all reachable nodes.
///
/// Returns `idom: Vec<u32>` of length `n+1`.
/// `idom[n] = n` (virtual root dominates itself).
/// `idom[v] = n` when the virtual root is the immediate dominator of `v`.
/// Unreachable nodes retain `UNDEFINED`.
pub fn compute_dominators(
    n: usize,
    rpo: &RpoResult,
    gc_root_indices: &[u32],
    inb_offsets: &[u32],  // byte offsets into inb_data, len = n+1
    inb_data: &[u8],
) -> Vec<u32> {
    let vroot = n as u32;

    let mut idom = vec![UNDEFINED; n + 1];
    // Virtual root dominates itself
    idom[n] = vroot;

    // Pre-seed GC roots and build vr_adjacent bitset
    let mut vr_adjacent = vec![false; n + 1];
    for &r in gc_root_indices {
        vr_adjacent[r as usize] = true;
        idom[r as usize] = vroot;
    }

    let mut changed = true;
    let mut iter = 0usize;
    while changed {
        changed = false;
        iter += 1;
        if iter > n + 10 {
            eprintln!("dominator: convergence loop limit exceeded (n={n}, iter={iter})");
            break;
        }

        // Iterate ALL nodes in RPO order (rpo_order contains only real nodes, vroot excluded)
        for rpo_idx in 0..rpo.rpo_order.len() {
            let b = rpo.rpo_order[rpo_idx] as usize;

            let mut new_idom: u32 = if vr_adjacent[b] { vroot } else { UNDEFINED };

            // Decode inbound predecessor list for node b
            let byte_start = inb_offsets[b] as usize;
            let byte_end   = inb_offsets[b + 1] as usize;
            let mut pos = byte_start;
            let mut prev: u32 = 0;

            while pos < byte_end {
                let (delta, consumed) = vbyte::decode_one(&inb_data[pos..]);
                pos += consumed;
                let pred_raw = prev.wrapping_add(delta);
                prev = pred_raw;

                // Strip excluded-edge marker (high bit) — CHK processes all predecessors
                let pred = (pred_raw & 0x7fff_ffff) as usize;

                // Skip if pred is not yet processed / unreachable
                if idom[pred] == UNDEFINED {
                    continue;
                }
                // Skip if pred's RPO position is unvisited or in-progress
                if rpo.rpo_pos[pred] < 0 || rpo.rpo_pos[pred] == i32::MAX {
                    continue;
                }

                if new_idom == UNDEFINED {
                    new_idom = pred as u32;
                } else {
                    new_idom = intersect(pred as u32, new_idom, &idom, &rpo.rpo_pos, n);
                }
            }

            if new_idom != UNDEFINED && idom[b] != new_idom {
                idom[b] = new_idom;
                changed = true;
            }
        }
    }

    idom
}

/// CHK intersect: walk both fingers up the dominator tree until they meet.
///
/// The virtual root (index `n`) has `rpo_pos[n] = 0`, same as `rpo_order[0]`.
/// We use a max_steps guard to prevent infinite loops on malformed graphs.
fn intersect(mut b1: u32, mut b2: u32, idom: &[u32], rpo_pos: &[i32], n: usize) -> u32 {
    let max_steps = idom.len();

    while b1 != b2 {
        let mut steps1 = 0usize;
        while rpo_pos[b1 as usize] > rpo_pos[b2 as usize] {
            steps1 += 1;
            if steps1 > max_steps {
                b1 = n as u32;
                break;
            }
            let next = idom[b1 as usize];
            if next == u32::MAX || next == b1 {
                b1 = n as u32;
                break;
            }
            b1 = next;
        }

        let mut steps2 = 0usize;
        while rpo_pos[b2 as usize] > rpo_pos[b1 as usize] {
            steps2 += 1;
            if steps2 > max_steps {
                b2 = n as u32;
                break;
            }
            let next = idom[b2 as usize];
            if next == u32::MAX || next == b2 {
                b2 = n as u32;
                break;
            }
            b2 = next;
        }

        if b1 == b2 {
            break;
        }
        // If both are at the same position but different nodes, bail to vroot
        if rpo_pos[b1 as usize] == rpo_pos[b2 as usize] && b1 != b2 {
            return n as u32;
        }
    }
    b1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpo_dfs::rpo_dfs;

    #[test]
    fn diamond_idom() {
        use crate::rpo_dfs::rpo_dfs;
        // vroot→0, 0→{1,2}, 1→3, 2→3
        let fwd_off = vec![0u32, 2, 3, 4, 4];
        let fwd_tgt = vec![1u32, 2, 3, 3];
        let roots = vec![0u32];
        let rpo = rpo_dfs(4, &roots, &fwd_off, &fwd_tgt);
        // inbound CSR: node1←[0], node2←[0], node3←[1,2]
        let inb_offsets = vec![0u32, 0, 1, 2, 4];
        let inb_data = vec![0x00u8, 0x00u8, 0x01u8, 0x01u8]; // delta-encoded
        let idom = compute_dominators(4, &rpo, &roots, &inb_offsets, &inb_data);
        assert_eq!(idom[0], 4, "idom[0]=vroot"); // gc root
        assert_eq!(idom[1], 0, "idom[1]=0");
        assert_eq!(idom[2], 0, "idom[2]=0");
        assert_eq!(idom[3], 0, "idom[3]=0 (both paths go through 0)");
    }

    #[test]
    fn chain_idom() {
        use crate::rpo_dfs::rpo_dfs;
        let fwd_off = vec![0u32, 1, 2, 2];
        let fwd_tgt = vec![1u32, 2u32];
        let roots = vec![0u32];
        let rpo = rpo_dfs(3, &roots, &fwd_off, &fwd_tgt);
        let inb_offsets = vec![0u32, 0, 1, 2];
        let inb_data = vec![0x00u8, 0x01u8];
        let idom = compute_dominators(3, &rpo, &roots, &inb_offsets, &inb_data);
        assert_eq!(idom[0], 3); // vroot
        assert_eq!(idom[1], 0);
        assert_eq!(idom[2], 1);
    }

    #[test]
    fn two_roots_no_shared_path() {
        // vroot→{0,1}; 0 and 1 have no edges between them.
        let fwd_off = vec![0u32, 0, 0];
        let fwd_tgt = vec![];
        let roots = vec![0u32, 1u32];
        let rpo = rpo_dfs(2, &roots, &fwd_off, &fwd_tgt);
        let inb_offsets = vec![0u32, 0, 0];
        let inb_data = vec![];
        let idom = compute_dominators(2, &rpo, &roots, &inb_offsets, &inb_data);
        let vroot = 2u32;
        assert_eq!(idom[0], vroot);
        assert_eq!(idom[1], vroot);
    }
}
