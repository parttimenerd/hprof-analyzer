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
    /// `rpo_pos[v]` = position of `v` in `rpo_order`.
    /// -1 means unreachable / unvisited.
    /// `i32::MAX` is used as an in-progress sentinel during DFS.
    /// Length = n + 1 (slot `n` holds the virtual root, set to 0 after build).
    pub rpo_pos: Vec<i32>,
    /// `dfs_parent[v]` = DFS-tree parent of `v`.
    /// `n` means the virtual root is the parent.
    /// `u32::MAX` means unvisited / unset.
    /// Length = n + 1.
    pub dfs_parent: Vec<u32>,
}

pub fn rpo_dfs(n: usize, roots: &[u32], fwd_off: &[u32], fwd_tgt: &[u32]) -> RpoResult {
    let vroot = n as u32;

    // rpo_pos: -1 = unvisited, i32::MAX = in-progress, >=0 = final position
    let mut rpo_pos = vec![-1i32; n + 1];
    let mut dfs_parent = vec![u32::MAX; n + 1];
    let mut post_order: Vec<u32> = Vec::with_capacity(n + 1);

    // Explicit stacks: parallel arrays (node, child_cursor)
    let mut node_stack: Vec<u32> = Vec::with_capacity(1024);
    let mut cursor_stack: Vec<usize> = Vec::with_capacity(1024);

    // Push virtual root
    dfs_parent[n] = vroot; // self-loop: vroot's parent is itself
    rpo_pos[n] = i32::MAX; // in-progress
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

            if rpo_pos[child as usize] == -1 {
                // Unvisited: push onto stack
                rpo_pos[child as usize] = i32::MAX; // in-progress
                dfs_parent[child as usize] = top;
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

    // RPO = reverse of post-order, excluding virtual root
    let rpo_order: Vec<u32> = post_order
        .iter()
        .rev()
        .filter(|&&v| v != vroot)
        .copied()
        .collect();

    // Assign final RPO positions (0-indexed)
    // After this loop real nodes have positions 0..rpo_order.len()-1
    for (i, &v) in rpo_order.iter().enumerate() {
        rpo_pos[v as usize] = i as i32;
    }
    // Virtual root is conceptually at RPO position 0 (before all real nodes)
    rpo_pos[n] = 0;

    RpoResult { rpo_order, rpo_pos, dfs_parent }
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
        assert_eq!(r.rpo_pos[2], -1);
    }

    #[test]
    fn rpo_pos_consistent() {
        // rpo_pos[v] should equal the index of v in rpo_order for real nodes
        let fwd_off = vec![0u32, 2, 3, 4, 4];
        let fwd_tgt = vec![1u32, 2, 3, 3];
        let r = rpo_dfs(4, &[0u32], &fwd_off, &fwd_tgt);
        for (pos, &v) in r.rpo_order.iter().enumerate() {
            assert_eq!(r.rpo_pos[v as usize], pos as i32);
        }
    }
}
