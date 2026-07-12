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
    rpo: RpoResult,
    gc_root_indices: &[u32],
    inb_block_off: &[u64], // blocked byte offsets: one per INB_BLOCK nodes + sentinel
    inb_data: &[u8],
) -> std::io::Result<Vec<u32>> {
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
    crate::trace::probe("dominator: after semi/ancestor/label alloc");

    // Initialize: semi[i] = i, label[i] = i, parent[i] = parent_pre[i]
    for i in 0..count {
        semi[i] = i as u32;
        label[i] = i as u32;
        ancestor[i] = 0;
    }
    crate::trace::probe("dominator: after init loop (semi/ancestor/label resident)");

    // vr_adjacent: which nodes have an implicit virtual-root predecessor (GC roots).
    // For these, the virtual root is always a predecessor in the eval step.
    let mut vr_adjacent = crate::bitset::Bitset::with_len(n + 1);
    for &r in gc_root_indices {
        vr_adjacent.set(r as usize);
    }

    // ── Phase 1 with a three-tier recovery ladder ─────────────────
    // The blocked CSR decode is the sole source of predecessor lists here. If a
    // framing desync ever slips past the u32-offset fix (e.g. transient memory
    // corruption, or a latent logic bug on some future dump), a garbage
    // `pred_pre >= count` would index the work arrays out of bounds and panic.
    // Instead we bounds-check the decode and recover:
    //   Tier 0 (normal): blocked decode — fast, seeks to the block start and
    //                     scan-skips preceding nodes.
    //   Tier 1: re-run Phase 1 with the same blocked decode. Recovers transient
    //           (non-deterministic) corruption at no memory cost.
    //   Tier 2: rebuild an exact per-node offset index by a single sequential
    //           scan of inb_data (independent of inb_block_off), then re-run
    //           Phase 1 with O(1) lookups. Recovers a systematic block-offset
    //           desync. Costs one (count+1)*8-byte index during the retry only.
    //   Exhausted: return an io::Error so the caller exits cleanly with a clear
    //              message rather than emitting a silently-wrong dominator tree.
    let mut recovered_index: Option<Vec<u64>> = None;
    let mut attempt = 0u32;
    loop {
        // Reset the union-find work arrays for this attempt.
        for i in 0..count {
            semi[i] = i as u32;
            label[i] = i as u32;
            ancestor[i] = 0;
        }
        let res = phase1(
            count,
            &rpo,
            &vr_adjacent,
            inb_block_off,
            inb_data,
            recovered_index.as_deref(),
            &mut semi,
            &mut ancestor,
            &mut label,
        );
        match res {
            Ok(()) => break,
            Err(desync) => {
                attempt += 1;
                if attempt == 1 {
                    eprintln!(
                        "[dominator] inbound CSR decode desync at pre-order {} ({}); \
                         retrying (tier 1)",
                        desync.at, desync.why
                    );
                    // Tier 1: retry the blocked decode as-is (handles transient
                    // corruption). recovered_index stays None.
                    continue;
                } else if attempt == 2 {
                    eprintln!(
                        "[dominator] desync persisted at pre-order {} ({}); rebuilding \
                         exact offset index and retrying (tier 2)",
                        desync.at, desync.why
                    );
                    // Tier 2: sequential scan builds an exact per-node byte
                    // offset for every reachable node, bypassing inb_block_off.
                    recovered_index = Some(build_exact_offsets(n, inb_data)?);
                    continue;
                } else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "inbound CSR decode desync at pre-order {} ({}); recovery \
                             exhausted, cannot compute a valid dominator tree",
                            desync.at, desync.why
                        ),
                    ));
                }
            }
        }
    }
    drop(recovered_index);

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

    // `semi` is dead after the SEMI-NCA loop above (its last read is the
    // `d > semi[i]` guard). Free it before allocating `idom` so the ~2GB
    // (count*4 @514M) region can back the new (n+1)*4 `idom` array in place,
    // rather than adding a fresh 2GB on top of the dominator-window peak.
    drop(semi);
    crate::trace::probe("dominator: after drop(semi), before idom alloc");

    // ── Translate pre-order idom back to node-index space ────────────────
    let mut idom = vec![UNDEFINED; n + 1];
    crate::trace::probe("dominator: after idom alloc");
    idom[n] = vroot; // virtual root dominates itself
    for i in 1..count {
        let node = rpo.vertex[i] as usize;
        let dom_pre = idom_pre[i];
        let dom_node = rpo.vertex[dom_pre as usize];
        // If the dominator is the virtual root (pre-order 0), store vroot (= n)
        idom[node] = if dom_pre == 0 { vroot } else { dom_node };
    }

    Ok(idom)
}

/// A decode desync detected during Phase 1: `pos` walked past the end of
/// `inb_data`, or a decoded predecessor pre-order was >= count (out of range).
struct DesyncErr {
    at: usize, // pre-order index i being processed when the desync was seen
    why: &'static str,
}

/// Bounds-checked vbyte decode: returns Err if the value would read past the
/// end of `buf`. Used only inside Phase 1 recovery-aware decode.
#[inline]
fn decode_checked(buf: &[u8], pos: usize) -> Option<(u32, usize)> {
    if pos > buf.len() {
        return None;
    }
    let (v, c) = vbyte::decode_one(&buf[pos..]);
    // decode_one returns consumed == buf.len()-pos with the high bit still set
    // when it ran off the end without a terminator; detect that.
    if pos + c > buf.len() {
        return None;
    }
    Some((v, c))
}

/// Run Phase 1 (semidominator computation) over the reverse pre-order.
/// Decodes each node's predecessor list from the blocked CSR (or, when
/// `exact_offsets` is Some, from an exact per-node byte-offset table that
/// bypasses `inb_block_off`). Returns Err(DesyncErr) on any decode
/// inconsistency instead of panicking, so the caller can recover.
#[allow(clippy::too_many_arguments)]
fn phase1(
    count: usize,
    rpo: &RpoResult,
    vr_adjacent: &crate::bitset::Bitset,
    inb_block_off: &[u64],
    inb_data: &[u8],
    exact_offsets: Option<&[u64]>,
    semi: &mut [u32],
    ancestor: &mut [u32],
    label: &mut [u32],
) -> Result<(), DesyncErr> {
    let cnt_u32 = count as u32;
    for i in (1..count).rev() {
        let w_node = rpo.vertex[i] as usize;

        let process_pred = |pv: u32,
                            semi: &mut [u32],
                            ancestor: &mut [u32],
                            label: &mut [u32]|
         -> Result<(), DesyncErr> {
            // Guard: a valid predecessor pre-order is < count. A garbage
            // value here is the exact class of bug that previously indexed
            // the work arrays out of bounds.
            if pv >= cnt_u32 {
                return Err(DesyncErr {
                    at: i,
                    why: "predecessor pre-order out of range",
                });
            }
            let u = eval(pv, ancestor, label, semi);
            if semi[u as usize] < semi[i] {
                semi[i] = semi[u as usize];
            }
            Ok(())
        };

        // Implicit virtual-root predecessor for GC roots
        if vr_adjacent.get(w_node) {
            let u = eval(0, ancestor, label, semi);
            if semi[u as usize] < semi[i] {
                semi[i] = semi[u as usize];
            }
        }

        // Seek to w's slice. Either directly (exact per-node offsets) or by
        // seeking to the block start and scan-skipping preceding nodes.
        let mut pos = if let Some(off) = exact_offsets {
            off[w_node] as usize
        } else {
            let block = w_node / crate::pass2::INB_BLOCK;
            let mut p = inb_block_off[block] as usize;
            for _ in (block * crate::pass2::INB_BLOCK)..w_node {
                let (cnt, c0) = decode_checked(inb_data, p).ok_or(DesyncErr {
                    at: i,
                    why: "skip count ran off end",
                })?;
                p += c0;
                for _ in 0..cnt {
                    let (_, c1) = decode_checked(inb_data, p).ok_or(DesyncErr {
                        at: i,
                        why: "skip delta ran off end",
                    })?;
                    p += c1;
                }
            }
            p
        };

        let (cnt_w, c0) = decode_checked(inb_data, pos).ok_or(DesyncErr {
            at: i,
            why: "count ran off end",
        })?;
        pos += c0;
        let mut prev: u32 = 0;
        for _ in 0..cnt_w {
            let (delta, consumed) = decode_checked(inb_data, pos).ok_or(DesyncErr {
                at: i,
                why: "delta ran off end",
            })?;
            pos += consumed;
            let pred_pre = prev.wrapping_add(delta);
            prev = pred_pre;
            process_pred(pred_pre, semi, ancestor, label)?;
        }

        link(i as u32, rpo.parent_pre[i], ancestor);
    }
    Ok(())
}

/// Tier-2 recovery: rebuild an exact per-node byte-offset index by a single
/// sequential scan of `inb_data`, using only the self-delimiting count-prefix
/// framing (independent of the possibly-corrupt `inb_block_off`). Returns a
/// Vec of length n+1: offsets[node] = byte offset of that node's slice, with a
/// trailing sentinel. Costs (n+1)*8 bytes for the duration of the retry only.
fn build_exact_offsets(n: usize, inb_data: &[u8]) -> std::io::Result<Vec<u64>> {
    let mut offsets = Vec::with_capacity(n + 1);
    let mut pos = 0usize;
    for node in 0..n {
        offsets.push(pos as u64);
        let (cnt, c0) = decode_checked(inb_data, pos).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("CSR rebuild: count ran off end at node {node}"),
            )
        })?;
        pos += c0;
        for _ in 0..cnt {
            let (_, c1) = decode_checked(inb_data, pos).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("CSR rebuild: delta ran off end at node {node}"),
                )
            })?;
            pos += c1;
        }
    }
    offsets.push(pos as u64); // trailing sentinel = total bytes consumed
    if pos != inb_data.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "CSR rebuild consumed {} of {} bytes; framing is corrupt",
                pos,
                inb_data.len()
            ),
        ));
    }
    Ok(offsets)
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

    // Build inbound CSR in the production BLOCKED format: predecessors in
    // PRE-ORDER space (node -> dfn, drop unreachable, sort+dedup), each node's
    // slice count-prefixed, one sampled byte-offset per INB_BLOCK nodes + a
    // trailing sentinel. Returns (inb_block_off, inb_data).
    fn build_inb(n: usize, preds: &[Vec<u32>], dfn: &[u32]) -> (Vec<u64>, Vec<u8>) {
        use crate::pass2::INB_BLOCK;
        let mut block_off = Vec::with_capacity(n / INB_BLOCK + 2);
        let mut data = Vec::new();
        for i in 0..n {
            let mut pre: Vec<u32> = preds
                .get(i)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|node| dfn[node as usize])
                .filter(|&p| p != u32::MAX)
                .collect();
            pre.sort_unstable();
            pre.dedup();
            if i % INB_BLOCK == 0 {
                block_off.push(data.len() as u64);
            }
            vbyte::encode(pre.len() as u32, &mut data);
            let mut prev = 0u32;
            for &v in &pre {
                vbyte::encode(v - prev, &mut data);
                prev = v;
            }
        }
        block_off.push(data.len() as u64);
        (block_off, data)
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
        let (inb_offsets, inb_data) = build_inb(4, &preds, &rpo.dfn);
        let idom = compute_dominators(4, rpo, &roots, &inb_offsets, &inb_data).unwrap();
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
        let (inb_offsets, inb_data) = build_inb(3, &preds, &rpo.dfn);
        let idom = compute_dominators(3, rpo, &roots, &inb_offsets, &inb_data).unwrap();
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
        let (inb_offsets, inb_data) = build_inb(2, &preds, &rpo.dfn);
        let idom = compute_dominators(2, rpo, &roots, &inb_offsets, &inb_data).unwrap();
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
        let (inb_offsets, inb_data) = build_inb(4, &preds, &rpo.dfn);
        let idom = compute_dominators(4, rpo, &roots, &inb_offsets, &inb_data).unwrap();
        assert_eq!(idom[0], 4);
        assert_eq!(idom[1], 0);
        assert_eq!(idom[2], 0, "2 dominated by 0 (direct edge bypasses 1)");
        assert_eq!(idom[3], 0, "3 dominated by 0 (paths via 1 and via 2)");
    }

    // A chain long enough to span several INB_BLOCK blocks. Corrupting a
    // non-zero block offset forces a decode desync that the recovery ladder
    // must catch and repair (tier 2 rebuild), still producing the correct
    // dominator tree (in a chain, idom[i] = i-1).
    #[test]
    fn recovers_from_corrupt_block_offset() {
        use crate::pass2::INB_BLOCK;
        let n = INB_BLOCK * 3 + 5; // spans 4 blocks
        // Forward chain vroot->0->1->...->n-1
        let mut fwd_off = vec![0u32; n + 1];
        let mut fwd_tgt = Vec::new();
        for i in 0..n {
            fwd_off[i] = fwd_tgt.len() as u32;
            if i + 1 < n {
                fwd_tgt.push((i + 1) as u32);
            }
        }
        fwd_off[n] = fwd_tgt.len() as u32;
        let roots = vec![0u32];
        let rpo = rpo_dfs(n, &roots, &fwd_off, &fwd_tgt);
        // inbound: node i (i>=1) has predecessor i-1
        let mut preds = vec![Vec::new(); n];
        for i in 1..n {
            preds[i] = vec![(i - 1) as u32];
        }
        let (good_off, inb_data) = build_inb(n, &preds, &rpo.dfn);

        // Sanity: uncorrupted run is correct.
        let rpo2 = rpo_dfs(n, &roots, &fwd_off, &fwd_tgt);
        let idom_good = compute_dominators(n, rpo2, &roots, &good_off, &inb_data).unwrap();
        for i in 1..n {
            assert_eq!(idom_good[i], (i - 1) as u32, "good chain idom[{i}]");
        }

        // Corrupt block offset 1 (a non-zero block start) to a wrong value.
        let mut bad_off = good_off.clone();
        bad_off[1] = bad_off[1].wrapping_add(1); // shift into the middle of a vbyte
        let rpo3 = rpo_dfs(n, &roots, &fwd_off, &fwd_tgt);
        let idom_rec = compute_dominators(n, rpo3, &roots, &bad_off, &inb_data).unwrap();
        for i in 1..n {
            assert_eq!(idom_rec[i], (i - 1) as u32, "recovered chain idom[{i}]");
        }
        assert_eq!(idom_rec, idom_good, "recovery reproduces the correct tree");
    }
}
