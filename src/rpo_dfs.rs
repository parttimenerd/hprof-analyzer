/// Iterative DFS that computes Reverse Post-Order (RPO) for a graph with a
/// virtual root node at index `n`.  The virtual root's children are given by
/// `roots`; all other edges come from the forward CSR (`fwd_off`, `fwd_tgt`).
///
/// No recursion is used — real heaps have millions of nodes.
///
/// # Indexing convention
/// Objects are 0..n-1; the virtual root is index `n`.
/// Mirrors the Java `RpoDfs` in hprof-redact with the root/object index
/// convention flipped.

pub struct RpoResult {
    /// Real nodes (indices 0..n-1) in reverse post-order; virtual root excluded.
    pub rpo_order: Vec<u32>,
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
    pub vertex: Vec<u32>,
}

pub fn rpo_dfs(n: usize, roots: &[u32], fwd_off: &[u32], fwd_tgt: &[u32]) -> RpoResult {
    let vroot = n as u32;

    let mut parent_pre: Vec<u32> = Vec::with_capacity(n + 1);
    let mut dfn = vec![u32::MAX; n + 1];
    let mut vertex: Vec<u32> = Vec::with_capacity(n + 1);
    let mut dfs_count: u32 = 0;
    let mut post_order: Vec<u32> = Vec::with_capacity(n + 1);

    // Explicit stacks: parallel arrays (node, child_cursor)
    let mut node_stack: Vec<u32> = Vec::with_capacity(1024);
    let mut cursor_stack: Vec<usize> = Vec::with_capacity(1024);

    // Push virtual root (pre-order number 0)
    dfn[n] = dfs_count;
    vertex.push(vroot);
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
        while *cursor < child_count {
            let child: u32 = if top == vroot {
                roots[*cursor]
            } else {
                let v = top as usize;
                fwd_tgt[(fwd_off[v] as usize) + *cursor]
            };
            *cursor += 1;

            // Bounds check
            if child as usize >= n + 1 {
                continue;
            }

            if dfn[child as usize] == u32::MAX {
                // Unvisited: push onto stack, assign pre-order number
                dfn[child as usize] = dfs_count;
                vertex.push(child);
                parent_pre.push(dfn[top as usize]);
                dfs_count += 1;
                node_stack.push(child);
                cursor_stack.push(0);
                pushed = true;
                break;
            }
            // Already visited (in-progress or finished): skip
        }

        if !pushed {
            // All children processed → node finishes, add to post-order
            post_order.push(top);
            node_stack.pop();
            cursor_stack.pop();
        }
    }

    // RPO = reverse of post-order, excluding the virtual root. The vroot
    // finishes last, so it is the final entry of ; reversing in
    // place puts it first, and truncating drops it — no second n-length
    // allocation (reuses the post_order buffer as rpo_order).
    debug_assert_eq!(post_order.last().copied(), Some(vroot));
    post_order.pop(); // drop vroot (finishes last) — O(1), no element shift
    post_order.reverse();
    let rpo_order: Vec<u32> = post_order;

    RpoResult { rpo_order, parent_pre, dfn, vertex }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpo_diamond() {
        // 0→{1,2}, 1→3, 2→3; roots=[0]
        let fwd_off = vec![0u32, 2, 3, 4, 4];
        let fwd_tgt = vec![1u32, 2, 3, 3];
        let r = rpo_dfs(4, &[0u32], &fwd_off, &fwd_tgt);
        assert_eq!(r.rpo_order.len(), 4); // all 4 nodes reachable
        let p3 = r.rpo_order.iter().position(|&x| x == 3).unwrap();
        let p1 = r.rpo_order.iter().position(|&x| x == 1).unwrap();
        let p2 = r.rpo_order.iter().position(|&x| x == 2).unwrap();
        assert!(p3 > p1, "node 3 must come after node 1 in RPO");
        assert!(p3 > p2, "node 3 must come after node 2 in RPO");
    }

    #[test]
    fn rpo_unreachable() {
        // 0→1; node 2 unreachable; roots=[0]
        let fwd_off = vec![0u32, 1, 1, 1];
        let fwd_tgt = vec![1u32];
        let r = rpo_dfs(3, &[0u32], &fwd_off, &fwd_tgt);
        assert_eq!(r.rpo_order.len(), 2);
        // node 2 unreachable → never assigned a DFS number
        assert_eq!(r.dfn[2], u32::MAX);
    }

}
