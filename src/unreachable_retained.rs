//! Retained size *within the unreachable forest*.
//!
//! Eclipse MAT discards unreachable objects entirely. We go further: we compute
//! the retained size of each unreachable object *among the other unreachable
//! objects* — i.e. the shallow heap that would be freed with it if the whole
//! unreachable subgraph were the heap. This surfaces the real "full size" of
//! garbage subtrees (a leaked cache that is itself unreachable, say), grouped by
//! the class of the dominating object.
//!
//! ## Why this runs in `main.rs` right after `rpo_dfs`
//!
//! The pass needs the forward (out-edge) CSR, which is freed at the inbound
//! transpose. The only window where the forward CSR *and* reachability (`dfn`)
//! coexist is between `rpo_dfs` and the inbound build, so the pass runs there
//! and hands the report phase only a bounded per-class aggregate.
//!
//! ## Memory
//!
//! Only unreachable nodes are materialized. On the reference dumps unreachable
//! objects are 0.4%–9.8% of the heap, so the compact sub-CSR and the per-node
//! work arrays are a small fraction of `n`. `shallow`/`class_idx` are streamed
//! out of their compressed blobs (never re-inflated dense) into arrays indexed
//! by the dense unreachable id. The node→dense-id map is *not* an `n`-sized
//! array: it is a binary search over the sorted `orig` list (length `u`), and
//! the "is this node unreachable" test reuses the caller's `dfn` (`== u32::MAX`).
//! So the pass adds no allocation proportional to the whole heap.
//!
//! ## Algorithm
//!
//! A self-contained Cooper–Harvey–Kennedy iterative dominator pass over the
//! compact unreachable sub-CSR. A synthetic root (dense index `u`) is the parent
//! of every *garbage root* — an unreachable node with no unreachable predecessor
//! (the entry points of each garbage subtree). Retained size then folds up the
//! resulting dominator tree in a single post-order, exactly like
//! [`crate::retained`] does for the reachable graph.

use crate::chunkvec::ChunkU32;
use crate::cvec::CompressedU32;
use crate::report::UnreachableGarbageRoot;
use std::collections::HashMap;

/// Top-N garbage roots to emit in the tree view.
pub const GARBAGE_ROOTS_CAP: usize = 10;
/// Maximum depth of each garbage-root subtree (synthetic root = depth 0).
pub const GARBAGE_ROOT_DEPTH: usize = 6;
/// Maximum children emitted per node in the tree.
pub const GARBAGE_ROOT_FAN: usize = 8;

/// Per-class retained aggregate for the unreachable forest.
#[derive(Debug, Default, Clone)]
pub struct UnreachableRetained {
    /// `retained_by_class[class_idx]` = shallow heap dominated (within the
    /// unreachable forest) by objects whose class is `class_idx`, summed over
    /// all such objects, minus double counting via `has_same_class` — matching
    /// the reachable histogram's convention (see [`crate::retained`]).
    pub retained_by_class: HashMap<u32, u64>,
    /// Total retained heap in the forest. Equals the sum of shallow sizes of all
    /// unreachable objects (every node is dominated by exactly one garbage root
    /// within the forest), i.e. `unreachable_shallow`.
    pub total: u64,
    /// Top garbage-root dominator subtrees, sorted retained-desc, capped at
    /// `GARBAGE_ROOTS_CAP` roots × `GARBAGE_ROOT_DEPTH` levels × `GARBAGE_ROOT_FAN` children.
    pub garbage_roots: Vec<UnreachableGarbageRoot>,
}

/// Compute retained sizes within the unreachable forest.
///
/// * `n`            - number of real objects.
/// * `dfn`          - reverse-post-order numbers from `rpo_dfs`; `u32::MAX` marks
///   an unreachable node. Length `n + 1` (index `n` is the virtual root).
/// * `fwd_off`      - forward CSR offsets (length `n + 1`).
/// * `fwd_tgt`      - forward CSR targets.
/// * `shallow_c`    - compressed per-object shallow sizes (streamed).
/// * `class_idx_c`  - compressed per-object class indices (streamed).
/// * `class_count`  - number of distinct classes.
/// * `class_obj_class_idx` - class-object index → class-histogram row (sparse).
/// * `class_names`  - raw class name strings for building the garbage-root tree.
///
/// Returns `None` when there are no unreachable objects (caller skips the stage).
pub fn compute_unreachable_retained(
    n: usize,
    dfn: &[u32],
    fwd_off: &[u32],
    fwd_tgt: &ChunkU32,
    shallow_c: &CompressedU32,
    class_idx_c: &CompressedU32,
    class_count: usize,
    class_obj_class_idx: &HashMap<u32, u32>,
    class_names: &[String],
) -> std::io::Result<Option<UnreachableRetained>> {
    let undef = u32::MAX;

    // ── Step 1: enumerate unreachable nodes (dense id -> original id) ─────────
    // `orig` is the sorted list of unreachable node ids; `dfn[node] == undef`
    // *is* the "unreachable" predicate, so we need no `dense: Vec<u32>` array of
    // size `n` (that array alone was ~n*4 bytes — the pass's whole RSS overhead).
    // Because `dfn` is scanned in ascending node order, `orig` is strictly
    // ascending, so the dense id of a node is `orig.binary_search(&node)`.
    let mut orig: Vec<u32> = Vec::new(); // dense id -> original id
    for (node, &d) in dfn.iter().take(n).enumerate() {
        if d == undef {
            orig.push(node as u32);
        }
    }
    let u = orig.len();
    if u == 0 {
        return Ok(None);
    }
    // node -> dense id via binary search over the sorted `orig` (O(log u), no
    // n-sized allocation). Only called for intra-forest edge targets.
    let dense_of = |node: u32| -> Option<u32> { orig.binary_search(&node).ok().map(|i| i as u32) };

    // ── Step 2: stream shallow + class_idx for unreachable nodes only ─────────
    // A lockstep cursor over the sorted `orig` routes each streamed value to its
    // dense slot in O(1) (sequential, cache-friendly) — no per-node index array.
    let mut u_shallow = vec![0u32; u];
    let mut total: u64 = 0;
    {
        let mut di_cursor = 0usize;
        let mut node: usize = 0;
        shallow_c.for_each_u32(|s| {
            if di_cursor < u && orig[di_cursor] as usize == node {
                u_shallow[di_cursor] = s;
                total += s as u64;
                di_cursor += 1;
            }
            node += 1;
        })?;
    }
    let mut u_class = vec![undef; u];
    {
        let mut di_cursor = 0usize;
        let mut node: usize = 0;
        class_idx_c.for_each_u32(|c| {
            if di_cursor < u && orig[di_cursor] as usize == node {
                u_class[di_cursor] = c;
                di_cursor += 1;
            }
            node += 1;
        })?;
    }

    // ── Step 3: build the compact forward sub-CSR (unreachable -> unreachable) ─
    // An unreachable node can only point to reachable or unreachable nodes; edges
    // to reachable nodes are dropped (they never dominate within the forest). The
    // "is target unreachable" test is the free `dfn[tgt] == undef`; only kept
    // edges pay the `dense_of` binary search. The synthetic root (dense index
    // `u`) is wired separately in Step 4.
    let mut sub_off = vec![0u32; u + 1];
    for di in 0..u {
        let node = orig[di] as usize;
        let lo = fwd_off[node] as usize;
        let hi = fwd_off[node + 1] as usize;
        let mut deg = 0u32;
        for pos in lo..hi {
            let tgt = fwd_tgt.get(pos);
            if (tgt as usize) < n && dfn[tgt as usize] == undef {
                deg += 1;
            }
        }
        sub_off[di + 1] = deg;
    }
    for i in 0..u {
        sub_off[i + 1] += sub_off[i];
    }
    let total_edges = sub_off[u] as usize;
    let mut sub_tgt = vec![0u32; total_edges];
    {
        let mut cursor = sub_off.clone();
        for di in 0..u {
            let node = orig[di] as usize;
            let lo = fwd_off[node] as usize;
            let hi = fwd_off[node + 1] as usize;
            for pos in lo..hi {
                let tgt = fwd_tgt.get(pos);
                if (tgt as usize) < n && dfn[tgt as usize] == undef {
                    if let Some(dt) = dense_of(tgt) {
                        sub_tgt[cursor[di] as usize] = dt;
                        cursor[di] += 1;
                    }
                }
            }
        }
    }

    // ── Step 4: garbage roots = unreachable nodes with no unreachable in-edge ─
    // Mark nodes that are targets of some intra-forest edge; the rest are roots
    // and become direct children of the synthetic root.
    let mut has_pred = crate::bitset::Bitset::with_len(u);
    for &t in &sub_tgt {
        has_pred.set(t as usize);
    }
    let root = u as u32; // synthetic root dense index

    // ── Step 5: iterative Cooper–Harvey–Kennedy dominators over the sub-CSR ───
    // Predecessor CSR (transpose of sub_tgt) including the synthetic-root edges
    // to garbage roots, so every node is reachable from `root`.
    let idom = compute_idom_chk(u, &sub_off, &sub_tgt, &has_pred, root);

    // ── Step 6: retained fold up the dominator tree (post-order) ──────────────
    let (dc_off, dc_tgt) = build_children_csr(u, &idom, root);
    let retained = fold_retained(u, &u_shallow, &dc_off, &dc_tgt, root);

    // ── Step 7: per-class aggregate, mirroring the reachable histogram ────────
    // A node's retained counts toward its own class, unless an ancestor in the
    // dominator tree already has the same class (avoids double counting nested
    // same-class dominators — same rule as `retained.rs` / the class histogram).
    let mut retained_by_class: HashMap<u32, u64> = HashMap::new();
    let has_same = same_class_ancestor(u, &dc_off, &dc_tgt, &u_class, class_count, root);
    for di in 0..u {
        if has_same.get(di) {
            continue;
        }
        // Attribute to the node's own class; if it is a class object, attribute
        // to the class it represents (matching the reachable histogram rollup).
        let node = orig[di];
        let repr = class_obj_class_idx.get(&node).copied();
        let ci = match repr {
            Some(c) if (c as usize) < class_count => c,
            _ => {
                let c = u_class[di];
                if (c as usize) < class_count {
                    c
                } else {
                    continue;
                }
            }
        };
        *retained_by_class.entry(ci).or_insert(0) += retained[di];
    }

    Ok(Some(UnreachableRetained {
        retained_by_class,
        total,
        garbage_roots: build_garbage_root_trees(
            u,
            &idom,
            &has_pred,
            &retained,
            &dc_off,
            &dc_tgt,
            &u_class,
            class_names,
            root,
        ),
    }))
}

/// Build the capped garbage-root dominator tree for the report model.
/// Garbage roots are the direct children of the synthetic root (`!has_pred`).
/// Each subtree is walked depth-first up to `GARBAGE_ROOT_DEPTH` levels with
/// `GARBAGE_ROOT_FAN` children per node, sorted retained-desc.
fn build_garbage_root_trees(
    u: usize,
    idom: &[u32],
    has_pred: &crate::bitset::Bitset,
    retained: &[u64],
    dc_off: &[u32],
    dc_tgt: &[u32],
    u_class: &[u32],
    class_names: &[String],
    root: u32,
) -> Vec<UnreachableGarbageRoot> {
    // Collect subtree object counts once (post-order fold over dominator children).
    let mut subtree_objects = vec![1u64; u]; // each node counts itself
    // We need a post-order traversal; reuse the dominator-children CSR.
    // Build a topological ordering (parents before children in dc_off iteration
    // means we can fold in reverse RPO — just do a BFS from roots).
    {
        let mut queue: std::collections::VecDeque<u32> = (0..u as u32)
            .filter(|&n| !has_pred.get(n as usize))
            .collect();
        // BFS level-order; accumulate in reverse.
        let mut bfs_order: Vec<u32> = Vec::with_capacity(u);
        while let Some(node) = queue.pop_front() {
            bfs_order.push(node);
            for pos in dc_off[node as usize]..dc_off[node as usize + 1] {
                queue.push_back(dc_tgt[pos as usize]);
            }
        }
        // Fold children into parents in reverse BFS (post-order from leaves up).
        for &node in bfs_order.iter().rev() {
            let parent = idom[node as usize];
            if parent != root && parent != u32::MAX && (parent as usize) < u {
                subtree_objects[parent as usize] += subtree_objects[node as usize];
            }
        }
    }

    let make_node = |di: u32, depth: usize| -> UnreachableGarbageRoot {
        // Will be called recursively; use a stack-based approach below.
        let _ = (di, depth); // placeholder — real logic via recursive helper
        UnreachableGarbageRoot::default()
    };
    let _ = make_node; // suppress unused warning

    fn build_node(
        di: u32,
        depth: usize,
        dc_off: &[u32],
        dc_tgt: &[u32],
        retained: &[u64],
        subtree_objects: &[u64],
        u_class: &[u32],
        class_names: &[String],
    ) -> UnreachableGarbageRoot {
        let ci = u_class[di as usize] as usize;
        let pretty_class = if ci < class_names.len() {
            crate::report::pretty_class_name(&class_names[ci])
        } else {
            "<unknown>".to_string()
        };
        let children = if depth + 1 < GARBAGE_ROOT_DEPTH {
            let lo = dc_off[di as usize] as usize;
            let hi = dc_off[di as usize + 1] as usize;
            let mut kids: Vec<u32> = dc_tgt[lo..hi].to_vec();
            kids.sort_unstable_by(|&a, &b| retained[b as usize].cmp(&retained[a as usize]));
            kids.truncate(GARBAGE_ROOT_FAN);
            kids.iter()
                .map(|&child| {
                    build_node(
                        child,
                        depth + 1,
                        dc_off,
                        dc_tgt,
                        retained,
                        subtree_objects,
                        u_class,
                        class_names,
                    )
                })
                .collect()
        } else {
            vec![]
        };
        UnreachableGarbageRoot {
            pretty_class,
            retained: retained[di as usize],
            objects: subtree_objects[di as usize],
            children,
        }
    }

    // Collect garbage roots (nodes with no unreachable predecessor), sort by retained desc.
    let mut roots: Vec<u32> = (0..u as u32)
        .filter(|&n| !has_pred.get(n as usize))
        .collect();
    roots.sort_unstable_by(|&a, &b| retained[b as usize].cmp(&retained[a as usize]));
    roots.truncate(GARBAGE_ROOTS_CAP);

    roots
        .iter()
        .map(|&di| {
            build_node(
                di,
                0,
                dc_off,
                dc_tgt,
                retained,
                &subtree_objects,
                u_class,
                class_names,
            )
        })
        .collect()
}

/// Cooper–Harvey–Kennedy iterative dominators over the compact sub-CSR.
/// `root` (== `u`) is the synthetic root; nodes with no intra-forest predecessor
/// (`!has_pred`) are its direct children. Returns `idom` of length `u` in dense
/// space (`idom[root] == root`; the synthetic root is its own dominator).
fn compute_idom_chk(
    u: usize,
    sub_off: &[u32],
    sub_tgt: &[u32],
    has_pred: &crate::bitset::Bitset,
    root: u32,
) -> Vec<u32> {
    let undef = u32::MAX;

    // Predecessor CSR over dense nodes 0..u (synthetic-root edges excluded here;
    // the root's children are the `!has_pred` nodes, handled via `root` below).
    let mut pred_off = vec![0u32; u + 1];
    for &t in sub_tgt {
        pred_off[t as usize + 1] += 1;
    }
    for i in 0..u {
        pred_off[i + 1] += pred_off[i];
    }
    let mut pred = vec![0u32; pred_off[u] as usize];
    {
        let mut cursor = pred_off.clone();
        for s in 0..u {
            for pos in sub_off[s]..sub_off[s + 1] {
                let t = sub_tgt[pos as usize] as usize;
                pred[cursor[t] as usize] = s as u32;
                cursor[t] += 1;
            }
        }
    }

    // Reverse-post-order over the sub-CSR from the synthetic root (its children
    // are the `!has_pred` nodes). CHK converges fastest when nodes are processed
    // in RPO. `order` lists dense nodes in RPO; `rpo_num[node]` is its position.
    let mut order: Vec<u32> = Vec::with_capacity(u);
    let mut visited = crate::bitset::Bitset::with_len(u);
    // Iterative DFS producing post-order, then reverse.
    let mut stack: Vec<(u32, u32)> = Vec::new(); // (node, next child cursor)
    for start in 0..u {
        if has_pred.get(start) || visited.get(start) {
            continue;
        }
        visited.set(start);
        stack.push((start as u32, sub_off[start]));
        while let Some(&mut (node, ref mut cur)) = stack.last_mut() {
            if *cur < sub_off[node as usize + 1] {
                let child = sub_tgt[*cur as usize];
                *cur += 1;
                if !visited.get(child as usize) {
                    visited.set(child as usize);
                    stack.push((child, sub_off[child as usize]));
                }
            } else {
                order.push(node);
                stack.pop();
            }
        }
    }
    // Any node not reached from a garbage root (part of a pure cycle with no
    // entry) — append so it still gets an idom (its idom becomes root).
    for node in 0..u as u32 {
        if !visited.get(node as usize) {
            order.push(node);
        }
    }
    order.reverse();
    // rpo_num[root] = 0 (smallest); real nodes start at 1.
    let mut rpo_num = vec![undef; u + 1];
    rpo_num[root as usize] = 0;
    for (i, &node) in order.iter().enumerate() {
        rpo_num[node as usize] = i as u32 + 1;
    }

    // idom has u+1 entries; idom[root] = root terminates the intersect walk.
    let mut idom = vec![undef; u + 1];
    idom[root as usize] = root;
    // Seed: garbage roots' idom is the synthetic root.
    for node in 0..u {
        if !has_pred.get(node) {
            idom[node] = root;
        }
    }

    let intersect = |mut a: u32, mut b: u32, idom: &[u32], rpo_num: &[u32]| -> u32 {
        // Walk up in RPO order (higher rpo_num = deeper) until the fingers meet.
        while a != b {
            while rpo_num[a as usize] > rpo_num[b as usize] {
                a = idom[a as usize];
            }
            while rpo_num[b as usize] > rpo_num[a as usize] {
                b = idom[b as usize];
            }
        }
        a
    };

    let mut changed = true;
    while changed {
        changed = false;
        for &node in &order {
            if !has_pred.get(node as usize) {
                continue; // garbage root: idom already fixed to `root`.
            }
            let ni = node as usize;
            let mut new_idom = undef;
            for pos in pred_off[ni]..pred_off[ni + 1] {
                let p = pred[pos as usize];
                if idom[p as usize] == undef {
                    continue; // predecessor not yet processed
                }
                new_idom = if new_idom == undef {
                    p
                } else {
                    intersect(p, new_idom, &idom, &rpo_num)
                };
            }
            if new_idom != undef && idom[ni] != new_idom {
                idom[ni] = new_idom;
                changed = true;
            }
        }
    }
    idom
}

/// Build the dominator-children CSR from `idom` (dense space). The synthetic
/// `root`'s children are omitted (we never fold into the root). Returns
/// `(child_off, child_tgt)` with `child_off` length `u + 1`.
fn build_children_csr(u: usize, idom: &[u32], root: u32) -> (Vec<u32>, Vec<u32>) {
    let mut off = vec![0u32; u + 1];
    for node in 0..u {
        let p = idom[node];
        if p == u32::MAX || p == root || p == node as u32 {
            continue;
        }
        off[p as usize + 1] += 1;
    }
    for i in 0..u {
        off[i + 1] += off[i];
    }
    let mut tgt = vec![0u32; off[u] as usize];
    let mut cursor = off.clone();
    for node in 0..u {
        let p = idom[node];
        if p == u32::MAX || p == root || p == node as u32 {
            continue;
        }
        tgt[cursor[p as usize] as usize] = node as u32;
        cursor[p as usize] += 1;
    }
    (off, tgt)
}

/// Post-order fold of shallow sizes up the dominator tree: `retained[v]` = its
/// shallow plus the retained of every child. Iterative to avoid deep recursion.
fn fold_retained(
    u: usize,
    shallow: &[u32],
    dc_off: &[u32],
    dc_tgt: &[u32],
    _root: u32,
) -> Vec<u64> {
    let mut retained: Vec<u64> = shallow.iter().map(|&s| s as u64).collect();
    // Process in reverse topological order of the dominator tree. Compute a
    // post-order via iterative DFS from every node with no dominator-tree parent
    // among 0..u (garbage roots), then fold children into parents on pop.
    let mut visited = crate::bitset::Bitset::with_len(u);
    let mut stack: Vec<(u32, u32)> = Vec::new();
    // A node is a dominator-tree top (child of synthetic root) iff it has no
    // parent within 0..u — detectable as: it is not any node's dc child. We seed
    // from all nodes and rely on `visited` to process each subtree once.
    let mut is_child = crate::bitset::Bitset::with_len(u);
    for &c in dc_tgt {
        is_child.set(c as usize);
    }
    for start in 0..u {
        if is_child.get(start) || visited.get(start) {
            continue;
        }
        visited.set(start);
        stack.push((start as u32, dc_off[start]));
        while let Some(&mut (node, ref mut cur)) = stack.last_mut() {
            if *cur < dc_off[node as usize + 1] {
                let child = dc_tgt[*cur as usize];
                *cur += 1;
                if !visited.get(child as usize) {
                    visited.set(child as usize);
                    stack.push((child, dc_off[child as usize]));
                }
            } else {
                // Pop: fold this node's retained into its dominator-tree parent.
                stack.pop();
                if let Some(&(parent, _)) = stack.last() {
                    retained[parent as usize] += retained[node as usize];
                }
            }
        }
    }
    retained
}

/// `has_same[v]` = some proper dominator-tree ancestor of `v` shares `v`'s class.
/// Single DFS carrying a per-class "on current path" depth, mirroring
/// [`crate::retained::compute_retained`]'s hasSame logic (class-object case is
/// out of scope here — the sub-forest attribution already handles class objects
/// at the aggregate step).
fn same_class_ancestor(
    u: usize,
    dc_off: &[u32],
    dc_tgt: &[u32],
    class: &[u32],
    class_count: usize,
    _root: u32,
) -> crate::bitset::Bitset {
    let undef = u32::MAX;
    let mut has_same = crate::bitset::Bitset::with_len(u);
    let mut on_path: Vec<u32> = vec![0u32; class_count]; // depth+1 where class last seen; 0 = absent
    let mut is_child = crate::bitset::Bitset::with_len(u);
    for &c in dc_tgt {
        is_child.set(c as usize);
    }
    // Stack frames carry the saved on_path value to restore on pop.
    let mut stack: Vec<(u32, u32, u32)> = Vec::new(); // (node, child cursor, saved_on_path)
    for start in 0..u {
        if is_child.get(start) {
            continue;
        }
        push_frame(
            &mut stack,
            start as u32,
            dc_off,
            class,
            class_count,
            &mut on_path,
            &mut has_same,
        );
        while let Some(&mut (node, ref mut cur, _)) = stack.last_mut() {
            if *cur < dc_off[node as usize + 1] {
                let child = dc_tgt[*cur as usize];
                *cur += 1;
                push_frame(
                    &mut stack,
                    child,
                    dc_off,
                    class,
                    class_count,
                    &mut on_path,
                    &mut has_same,
                );
            } else {
                let (node, _, saved) = stack.pop().unwrap();
                let c = class[node as usize];
                if (c as usize) < class_count {
                    on_path[c as usize] = saved;
                }
                let _ = undef;
            }
        }
    }
    has_same
}

#[allow(clippy::too_many_arguments)]
fn push_frame(
    stack: &mut Vec<(u32, u32, u32)>,
    node: u32,
    dc_off: &[u32],
    class: &[u32],
    class_count: usize,
    on_path: &mut [u32],
    has_same: &mut crate::bitset::Bitset,
) {
    let depth = stack.len() as u32 + 1;
    let c = class[node as usize];
    let mut saved = 0u32;
    if (c as usize) < class_count {
        saved = on_path[c as usize];
        if saved > 0 {
            has_same.set(node as usize);
        }
        on_path[c as usize] = depth;
    }
    stack.push((node, dc_off[node as usize], saved));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cvec(v: &[u32]) -> CompressedU32 {
        CompressedU32::compress(v, crate::cvec::Codec::None).unwrap()
    }

    // Unreachable chain: nodes 2,3,4 unreachable (0,1 reachable).
    //   2 -> 3 -> 4, shallow [_, _, 10, 20, 30].
    // Expected forest: root -> 2 -> 3 -> 4. retained: 2=60, 3=50, 4=30.
    #[test]
    fn chain_forest_retained() {
        let n = 5;
        // dfn: reachable 0,1 (any != MAX); unreachable 2,3,4.
        let dfn = vec![0u32, 1, u32::MAX, u32::MAX, u32::MAX, 0 /*vroot*/];
        let fwd_off = vec![0u32, 0, 0, 1, 2, 2]; // node 2 ->[3], 3 ->[4], 4 ->[]
        let fwd_tgt = ChunkU32::from_vec(vec![3u32, 4u32]);
        let shallow = cvec(&[0, 0, 10, 20, 30]);
        let class_idx = cvec(&[0, 0, 1, 2, 3]);
        let r = compute_unreachable_retained(
            n,
            &dfn,
            &fwd_off,
            &fwd_tgt,
            &shallow,
            &class_idx,
            4,
            &HashMap::new(),
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.total, 60);
        // class 1 is node 2 (retains 60); no same-class ancestors.
        assert_eq!(r.retained_by_class.get(&1).copied(), Some(60));
        assert_eq!(r.retained_by_class.get(&2).copied(), Some(50));
        assert_eq!(r.retained_by_class.get(&3).copied(), Some(30));
    }

    // Disconnected forest: two independent garbage roots.
    //   5 -> 6, 7 -> 8; shallow all 10. Two roots (5 and 7).
    #[test]
    fn disconnected_forest() {
        let n = 9;
        let mut dfn = vec![0u32; n + 1];
        for d in dfn.iter_mut().take(5) {
            *d = 0; // 0..5 reachable
        }
        for d in dfn.iter_mut().take(9).skip(5) {
            *d = u32::MAX; // 5..9 unreachable
        }
        let fwd_off = vec![0u32, 0, 0, 0, 0, 0, 1, 1, 2, 2]; // 5->[6], 7->[8]
        let fwd_tgt = ChunkU32::from_vec(vec![6u32, 8u32]);
        let shallow = cvec(&[0, 0, 0, 0, 0, 10, 10, 10, 10]);
        let class_idx = cvec(&[0, 0, 0, 0, 0, 1, 1, 1, 1]);
        let r = compute_unreachable_retained(
            n,
            &dfn,
            &fwd_off,
            &fwd_tgt,
            &shallow,
            &class_idx,
            2,
            &HashMap::new(),
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.total, 40);
        // All class 1; roots 5 and 7 each retain 20, children skipped (same-class ancestor).
        assert_eq!(r.retained_by_class.get(&1).copied(), Some(40));
    }

    #[test]
    fn no_unreachable_returns_none() {
        let n = 2;
        let dfn = vec![0u32, 1, 0];
        let fwd_off = vec![0u32, 0, 0];
        let fwd_tgt = ChunkU32::from_vec(vec![]);
        let shallow = cvec(&[10, 20]);
        let class_idx = cvec(&[0, 0]);
        let r = compute_unreachable_retained(
            n,
            &dfn,
            &fwd_off,
            &fwd_tgt,
            &shallow,
            &class_idx,
            1,
            &HashMap::new(),
            &[],
        )
        .unwrap();
        assert!(r.is_none());
    }
}
