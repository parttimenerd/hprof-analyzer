/// Compute retained sizes and the hasSameClassAncestor bitset.
///
/// # Arguments
/// * `n`                   - number of real objects (vroot has index n)
/// * `rpo_order`           - object indices in reverse-post-order (roots first), len = reachable
/// * `idom`                - immediate dominator per node, len = n+1; idom[n]=n (vroot self-loop)
/// * `shallow`             - shallow size per object, len = n
/// * `class_idx`           - class index per object (into class_names), len = n
/// * `class_count`         - class_names.len()
/// * `class_obj_class_idx` - for each obj: which class it represents (u32::MAX if not class obj), len = n
///
/// # Returns
/// `(retained, has_same_class_ancestor)` both of length n.
pub fn compute_retained(
    n: usize,
    rpo_order: &[u32],
    idom: &[u32],
    shallow: &[u32],
    class_idx: &[u32],
    class_count: usize,
    class_obj_class_idx: &[u32],
) -> (Vec<u64>, Vec<bool>) {
    let vroot = n as u32;
    let undef = u32::MAX;

    // ── Retained size: reverse RPO accumulation ────────────────────────────
    // Initialize retained[v] = shallow[v] for all real objects.
    let mut retained: Vec<u64> = shallow.iter().map(|&s| s as u64).collect();

    // Process from last to first in rpo_order (leaves before parents).
    for &v in rpo_order.iter().rev() {
        let parent = idom[v as usize];
        // Skip unreachable (undef) or parent is vroot (no retained slot for vroot).
        if parent == undef || parent == vroot {
            continue;
        }
        retained[parent as usize] += retained[v as usize];
    }

    // ── hasSameClassAncestor: forward DFS of dominator tree ────────────────
    // Build children CSR from idom.
    // child_off has length n+2 to support indexing child_off[n+1] for vroot's child end.
    let mut child_deg: Vec<u32> = vec![0u32; n + 1];
    for u in 0..n {
        let p = idom[u];
        if p == undef || p == u as u32 {
            continue;
        }
        child_deg[p as usize] += 1;
    }

    let mut child_off: Vec<u32> = Vec::with_capacity(n + 2);
    child_off.push(0u32);
    for i in 0..=n {
        child_off.push(child_off[i] + child_deg[i]);
    }
    let total_children = *child_off.last().unwrap() as usize;
    let mut child_tgt: Vec<u32> = vec![u32::MAX; total_children];
    let mut cursor: Vec<u32> = child_off[..=n].to_vec();
    for u in 0..n {
        let p = idom[u];
        if p == undef || p == u as u32 {
            continue;
        }
        child_tgt[cursor[p as usize] as usize] = u as u32;
        cursor[p as usize] += 1;
    }

    // Iterative DFS over the dominator tree starting from vroot.
    let mut has_same = vec![false; n];

    // class_to_last_depth[c] = stack depth (sp) when class c was last pushed (0 = not on stack)
    // class_obj_depth[c]     = stack depth when class-object for class c was pushed (0 = not on stack)
    let mut class_to_last_depth: Vec<u32> = vec![0u32; class_count];
    let mut class_obj_depth: Vec<u32> = vec![0u32; class_count];

    // Parallel stacks for iterative DFS.
    let mut stk_node:            Vec<u32> = Vec::new();
    let mut stk_child_idx:       Vec<u32> = Vec::new();
    let mut stk_saved_depth:     Vec<u32> = Vec::new(); // saved class_to_last_depth value
    let mut stk_saved_obj_depth: Vec<u32> = Vec::new(); // saved class_obj_depth value
    let mut stk_cls:             Vec<u32> = Vec::new(); // class index of node (u32::MAX = vroot/none)
    let mut stk_ci:              Vec<u32> = Vec::new(); // class-obj class idx (u32::MAX = not a class obj)

    // Push virtual root (index n) to seed the DFS.
    stk_node.push(vroot);
    stk_child_idx.push(child_off[n]);
    stk_saved_depth.push(0);
    stk_saved_obj_depth.push(0);
    stk_cls.push(undef);
    stk_ci.push(undef);

    while !stk_node.is_empty() {
        let top = stk_node.len() - 1;
        let v = stk_node[top];
        let next_child_pos = stk_child_idx[top];
        // child_off[v+1] is safe: v is 0..=n and child_off has length n+2.
        let end_child = child_off[v as usize + 1];

        if next_child_pos < end_child {
            // Advance child iterator on the current frame.
            let child = child_tgt[next_child_pos as usize];
            stk_child_idx[top] = next_child_pos + 1;

            let cls = if (child as usize) < n { class_idx[child as usize] } else { undef };
            let ci  = if (child as usize) < n { class_obj_class_idx[child as usize] } else { undef };

            // sp_new = depth the child will have on the stack (1-based, vroot is depth 1).
            let sp_new = (stk_node.len() + 1) as u32;

            // Check and update class_to_last_depth for the child's own class.
            let saved_depth = if cls != undef && (cls as usize) < class_count {
                if class_to_last_depth[cls as usize] > 0 || class_obj_depth[cls as usize] > 0 {
                    has_same[child as usize] = true;
                }
                let sd = class_to_last_depth[cls as usize];
                class_to_last_depth[cls as usize] = sp_new;
                sd
            } else {
                0u32
            };

            // Check and update class_obj_depth for the class this object represents.
            let saved_obj_depth = if ci != undef && (ci as usize) < class_count {
                let sod = class_obj_depth[ci as usize];
                class_obj_depth[ci as usize] = sp_new;
                sod
            } else {
                0u32
            };

            // Push child frame.
            stk_node.push(child);
            stk_child_idx.push(child_off[child as usize]);
            stk_saved_depth.push(saved_depth);
            stk_saved_obj_depth.push(saved_obj_depth);
            stk_cls.push(cls);
            stk_ci.push(ci);
        } else {
            // All children of v processed — restore saved state and pop.
            let cls = stk_cls[top];
            let ci  = stk_ci[top];
            if cls != undef && (cls as usize) < class_count {
                class_to_last_depth[cls as usize] = stk_saved_depth[top];
            }
            if ci != undef && (ci as usize) < class_count {
                class_obj_depth[ci as usize] = stk_saved_obj_depth[top];
            }
            stk_node.pop();
            stk_child_idx.pop();
            stk_saved_depth.pop();
            stk_saved_obj_depth.pop();
            stk_cls.pop();
            stk_ci.pop();
        }
    }

    (retained, has_same)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Chain: vroot(3) → 0 → 1 → 2, shallow = [10, 20, 30]
    // idom = [3, 0, 1, 3]  (idom[3]=3 means vroot self-loop)
    #[test]
    fn chain_retained() {
        let n = 3;
        let rpo_order = vec![0u32, 1, 2];
        let idom = vec![3u32, 0, 1, 3];
        let shallow = vec![10u32, 20, 30];
        let class_idx = vec![0u32, 0, 0];
        let class_obj_class_idx = vec![u32::MAX, u32::MAX, u32::MAX];
        let (retained, _has_same) =
            compute_retained(n, &rpo_order, &idom, &shallow, &class_idx, 1, &class_obj_class_idx);
        assert_eq!(retained[0], 60, "0 retains all 3");
        assert_eq!(retained[1], 50, "1 retains 1+2");
        assert_eq!(retained[2], 30, "2 retains itself");
    }

    // Diamond: vroot(4) → 0, 0 → {1, 2}, 1 → 3, 2 → 3; idom[3] = 0
    #[test]
    fn diamond_retained() {
        let n = 4;
        let rpo_order = vec![0u32, 1, 2, 3];
        let idom = vec![4u32, 0, 0, 0, 4]; // idom[4]=4 vroot self-loop
        let shallow = vec![1u32, 2, 3, 4];
        let class_idx = vec![0u32, 0, 0, 0];
        let class_obj_class_idx = vec![u32::MAX; 4];
        let (retained, _) =
            compute_retained(n, &rpo_order, &idom, &shallow, &class_idx, 1, &class_obj_class_idx);
        // 3 propagates to 0, 1 propagates to 0, 2 propagates to 0
        // retained[0] = 1 + 2 + 3 + 4 = 10
        assert_eq!(retained[0], 10);
        assert_eq!(retained[1], 2);
        assert_eq!(retained[2], 3);
        assert_eq!(retained[3], 4);
    }

    // hasSameClassAncestor: chain where node 0 and node 2 have same class
    #[test]
    fn has_same_class_ancestor() {
        let n = 3;
        let rpo_order = vec![0u32, 1, 2];
        let idom = vec![3u32, 0, 1, 3];
        let shallow = vec![10u32, 20, 30];
        // class 0: nodes 0 and 2; class 1: node 1
        let class_idx = vec![0u32, 1, 0];
        let class_obj_class_idx = vec![u32::MAX; 3];
        let (_, has_same) =
            compute_retained(n, &rpo_order, &idom, &shallow, &class_idx, 2, &class_obj_class_idx);
        assert!(!has_same[0], "node 0 has no class-0 ancestor");
        assert!(!has_same[1], "node 1 has no class-1 ancestor");
        assert!(has_same[2], "node 2 has class-0 ancestor (node 0)");
    }

    // hasSameClassAncestor: class object is ancestor
    #[test]
    fn has_same_class_ancestor_via_class_obj() {
        // Objects:
        //   0: class object for class 1 (class_idx=0 = java/lang/Class, class_obj_class_idx=1)
        //   1: instance of class 1   (class_idx=1, class_obj_class_idx=MAX)
        //   2: instance of class 0   (class_idx=0, class_obj_class_idx=MAX)
        // Dominator tree: vroot(3) → 0 → 1 → 2
        let n = 3;
        let rpo_order = vec![0u32, 1, 2];
        let idom = vec![3u32, 0, 1, 3];
        let shallow = vec![10u32, 20, 30];
        let class_idx = vec![0u32, 1u32, 0u32];
        let class_obj_class_idx = vec![1u32, u32::MAX, u32::MAX];
        let (_, has_same) =
            compute_retained(n, &rpo_order, &idom, &shallow, &class_idx, 2, &class_obj_class_idx);
        assert!(!has_same[0], "node 0 has no ancestor of class 0 (nor class-obj for any class)");
        // node 1 has class 1; its ancestor node 0 is the class-object FOR class 1
        assert!(has_same[1], "node 1 has class-object-for-class-1 as ancestor");
        // node 2 has class 0; its ancestor node 0 also has class 0 → same class ancestor
        assert!(has_same[2], "node 2 has class-0 ancestor (node 0)");
    }
}
