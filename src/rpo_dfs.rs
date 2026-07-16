//! Iterative depth-first traversal of the object graph that assigns each
//! reachable node a DFS pre-order number and records its DFS-tree parent.
//! Feeds the SEMI-NCA dominator stage; the `vertex` permutation is rebuilt
//! lazily to keep it off the RSS peak.

/// Result of [`rpo_dfs`]: DFS pre-order numbering and tree structure for a
/// graph with a virtual root at index `n` whose children are `roots`; all
/// other edges come from the forward CSR (`fwd_off`, `fwd_tgt`).
///
/// No recursion is used — real heaps have millions of nodes.
///
/// # Indexing convention
/// Objects are 0..n-1; the virtual root is index `n`.
/// Mirrors the Java `RpoDfs` in hprof-analyzer with the root/object index
/// convention flipped.
pub struct RpoResult {
    /// `parent_pre[i]` = pre-order number of the DFS-tree parent of the node
    /// whose pre-order number is `i` (i.e. of `vertex[i]`).
    /// `parent_pre[0]` = 0 (virtual root's parent is itself).
    /// Length = number of reachable nodes + 1 (index 0 = virtual root),
    /// lockstep with `vertex`.
    pub parent_pre: Vec<u32>,
    /// DFS pre-order number of each node. `u32::MAX` = unvisited.
    /// Virtual root (index n) gets dfn 0; visited nodes get 1, 2, 3, ... in DFS pre-order.
    /// Length = n + 1. Used by SEMI-NCA dominator.
    pub dfn: Vec<u32>,
    /// Inverse of `dfn`: `vertex[i]` = node whose pre-order number is `i`.
    /// Length = number of reachable nodes + 1 (index 0 = virtual root).
    ///
    /// NOT built during the DFS: at 514M nodes this 1.96GB array is idle
    /// through the inbound 2b scan (the binding RSS peak — inb_flat + id_map +
    /// dfn + parent_pre + in_cursors all resident). `rpo_dfs` returns this
    /// EMPTY; the caller rebuilds it from `dfn` via [`rebuild_vertex`] AFTER
    /// inbound.build, just before the dominator stage that actually reads it.
    pub vertex: Vec<u32>,
}

/// Rebuild the `vertex` permutation (inverse of `dfn`) as a pure O(n) pass.
/// `count` = number of reachable nodes + 1 (== parent_pre.len()).
pub fn rebuild_vertex(dfn: &[u32], count: usize) -> Vec<u32> {
    let mut vertex = vec![0u32; count];
    for (node, &pre) in dfn.iter().enumerate() {
        if pre != u32::MAX {
            vertex[pre as usize] = node as u32;
        }
    }
    vertex
}

/// Traverse the graph from the virtual root, assigning DFS pre-order numbers
/// (`dfn`) and DFS-tree parents (`parent_pre`). `vertex` is returned empty;
/// see [`RpoResult::vertex`] and [`rebuild_vertex`].
pub fn rpo_dfs(
    n: usize,
    roots: &[u32],
    fwd_off: &[u32],
    fwd_tgt: &crate::chunkvec::ChunkU32,
) -> RpoResult {
    let vroot = n as u32;

    let mut parent_pre: Vec<u32> = Vec::with_capacity(n + 1);
    let mut dfn = vec![u32::MAX; n + 1];
    let mut dfs_count: u32 = 0;

    // Explicit stacks: parallel arrays (node, child_cursor)
    let mut node_stack: Vec<u32> = Vec::with_capacity(1024);
    let mut cursor_stack: Vec<usize> = Vec::with_capacity(1024);

    // Push virtual root (pre-order number 0)
    dfn[n] = dfs_count;
    parent_pre.push(0); // virtual root's parent is itself (pre-order 0)
    dfs_count += 1;
    node_stack.push(vroot);
    cursor_stack.push(0);

    while !node_stack.is_empty() {
        let top = *node_stack.last().unwrap();
        let cursor = cursor_stack.last_mut().unwrap();

        // Number of children for `top`
        let child_count: usize = if top == vroot {
            roots.len()
        } else {
            let v = top as usize;
            (fwd_off[v + 1] - fwd_off[v]) as usize
        };

        let mut pushed = false;
        if top == vroot {
            // Virtual root's children come from the `roots` slice directly.
            while *cursor < child_count {
                let child = roots[*cursor];
                *cursor += 1;
                if child as usize > n {
                    continue;
                }
                if dfn[child as usize] == u32::MAX {
                    dfn[child as usize] = dfs_count;
                    parent_pre.push(dfn[top as usize]);
                    dfs_count += 1;
                    node_stack.push(child);
                    cursor_stack.push(0);
                    pushed = true;
                    break;
                }
            }
        } else {
            let v = top as usize;
            let lo = fwd_off[v] as usize;
            let hi = fwd_off[v + 1] as usize;
            let parent_dfn = dfn[top as usize];
            // Fast path: adjacency list fits in one chunk — iterate the slice
            // directly without per-element shift/mask overhead.
            if let Some(adj) = fwd_tgt.range_slice(lo + *cursor, hi) {
                // Prefetch dfn[adj[k + PF_DFS]] while consuming adj[k].
                // dfn is a 2 GB random-access array; for nodes with many children
                // (large object arrays) most lookups are DRAM misses. PF_DFS=16
                // covers ~50 ns at 3 ns/iter to hide ~100 ns DRAM latency.
                const PF_DFS: usize = 16;
                let dfn_ptr = dfn.as_ptr();
                let dfn_len = dfn.len();
                for (k, &child) in adj.iter().enumerate() {
                    // Issue a prefetch for the element PF_DFS positions ahead.
                    if k + PF_DFS < adj.len() {
                        let pf_child = unsafe { *adj.get_unchecked(k + PF_DFS) } as usize;
                        if pf_child < dfn_len {
                            unsafe {
                                let ptr = dfn_ptr.add(pf_child) as *const i8;
                                #[cfg(target_arch = "x86_64")]
                                core::arch::x86_64::_mm_prefetch::<
                                    { core::arch::x86_64::_MM_HINT_T0 },
                                >(ptr);
                                #[cfg(target_arch = "aarch64")]
                                core::arch::asm!(
                                    "prfm pldl1keep, [{p}]",
                                    p = in(reg) ptr,
                                    options(nostack, readonly)
                                );
                            }
                        }
                    }
                    *cursor += 1;
                    if child as usize > n {
                        continue;
                    }
                    if dfn[child as usize] == u32::MAX {
                        dfn[child as usize] = dfs_count;
                        parent_pre.push(parent_dfn);
                        dfs_count += 1;
                        node_stack.push(child);
                        cursor_stack.push(0);
                        pushed = true;
                        break;
                    }
                }
            } else {
                // Cross-chunk fallback: use individual get() calls.
                while *cursor < child_count {
                    let child = fwd_tgt.get(lo + *cursor);
                    *cursor += 1;
                    if child as usize > n {
                        continue;
                    }
                    if dfn[child as usize] == u32::MAX {
                        dfn[child as usize] = dfs_count;
                        parent_pre.push(parent_dfn);
                        dfs_count += 1;
                        node_stack.push(child);
                        cursor_stack.push(0);
                        pushed = true;
                        break;
                    }
                }
            }
        }

        if !pushed {
            // All children processed → node finishes, pop it.
            node_stack.pop();
            cursor_stack.pop();
        }
    }

    RpoResult {
        parent_pre,
        dfn,
        // Rebuilt by the caller via rebuild_vertex() after inbound.build.
        vertex: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunkvec::ChunkU32;

    fn make_fwd_tgt(v: Vec<u32>) -> ChunkU32 {
        let mut c = ChunkU32::zeroed(v.len());
        for (i, &x) in v.iter().enumerate() {
            c.set(i, x);
        }
        c
    }

    #[test]
    fn rpo_diamond() {
        // 0→{1,2}, 1→3, 2→3; roots=[0]
        let fwd_off = vec![0u32, 2, 3, 4, 4];
        let fwd_tgt = make_fwd_tgt(vec![1u32, 2, 3, 3]);
        let r = rpo_dfs(4, &[0u32], &fwd_off, &fwd_tgt);
        // All 4 nodes reachable → each has a real pre-order number.
        for v in 0..4usize {
            assert_ne!(r.dfn[v], u32::MAX, "node {v} must be reachable");
        }
        // Pre-order from root 0: 0 first, then its subtree. Node 3 is reached
        // via node 1 (0's first child) before node 2 is opened, so
        // dfn[0] < dfn[1] < dfn[3] < dfn[2].
        assert!(r.dfn[0] < r.dfn[1], "root 0 visited before node 1");
        assert!(r.dfn[1] < r.dfn[3], "node 3 discovered via node 1");
        assert!(r.dfn[3] < r.dfn[2], "node 3 visited before node 2 (DFS)");
    }

    #[test]
    fn rpo_unreachable() {
        // 0→1; node 2 unreachable; roots=[0]
        let fwd_off = vec![0u32, 1, 1, 1];
        let fwd_tgt = make_fwd_tgt(vec![1u32]);
        let r = rpo_dfs(3, &[0u32], &fwd_off, &fwd_tgt);
        // nodes 0,1 reachable; node 2 unreachable (no DFS number).
        assert_ne!(r.dfn[0], u32::MAX);
        assert_ne!(r.dfn[1], u32::MAX);
        assert_eq!(r.dfn[2], u32::MAX);
    }
}
