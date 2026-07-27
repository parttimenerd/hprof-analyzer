//! Auto-escalated OQL execution path for cross-phase features.
//!
//! Shared between the CLI binary (`main.rs`) and the library crate (`lib.rs`).
//! Placed in its own module so `query/server.rs` can reference it as
//! `crate::run_oql::run_oql_escalated` regardless of which crate is being
//! compiled.

use std::io;

use crate::opts::{AnalyzeOptions, DEFAULT_QUERY_PATH_DEPTH};
use crate::{cvec, dominator, pass1, pass2, retained, rpo_dfs};
use crate::query;

/// A `ClassIndexResolver` that resolves nothing — used when only the boolean
/// `RunFlags` (retain_inbound/retain_forward/outbounds_by_rescan) are needed and
/// the dense class universe is unavailable (row filtering is done post-pass2 by
/// class-name match).
pub(crate) struct NoClassIndex;
impl query::runflags::ClassIndexResolver for NoClassIndex {
    fn class_bits(&self, _pattern: &str, _instanceof: bool) -> Vec<usize> {
        Vec::new()
    }
    fn universe_len(&self) -> usize {
        0
    }
}

/// True if this query (or any UNION branch) uses an edge feature
/// (`@inbounds` / `@outbounds` / `path()`).
pub(crate) fn query_uses_edges(q: &query::ast::Query) -> bool {
    query::runflags::plan_run(
        std::slice::from_ref(q),
        &NoClassIndex,
        DEFAULT_QUERY_PATH_DEPTH,
    )
    .map(|f| f.retain_inbound || f.retain_forward || f.outbounds_by_rescan)
    .unwrap_or(false)
}

/// The two query-gated edge structures built at the forward-CSR hook: the
/// forward store (`@outbounds`/`path`) and a bounded inbound `(in_off, in_tgt)`
/// CSR (`@inbounds`). Both `None` on a no-edge run.
pub(crate) type RetainedEdgeStructs = (
    Option<crate::query::retained_edges::RetainedEdges>,
    Option<(Vec<u32>, Vec<u32>)>,
);

/// Auto-escalated `query`-subcommand path for cross-phase OQL features
/// (@retainedHeapSize, dominators()/AS RETAINED SET, @inbounds/@outbounds/path,
/// @GCRoots/@GCRootInfo/@info, N-hop RefPath). Mirrors the `run()` analysis
/// pipeline's call sequence (pass1 → pass2 → rpo → inbound → dominators →
/// retained → resume) but SKIPS report generation, alloc-site aggregation,
/// unreachable-retained, and all the RSS-tuning compress/restore dance. It uses
/// `cvec::Codec::None` throughout so the dense arrays stay live — correctness
/// over peak memory (the query subcommand has no RSS contract). Returns the same
/// `Vec<QueryResult>` the fast path produces, so the caller's finalize/print loop
/// is unchanged. `reachable_only` governs final row pruning (skipped under `--all`).
pub(crate) fn run_oql_escalated(
    input: &str,
    flat: &[(query::ast::Query, query::plan::QueryPlan)],
    union_groups: &[query::run::UnionGroup],
    reachable_only: bool,
    opts: &AnalyzeOptions,
) -> io::Result<Vec<query::model::QueryResult>> {
    let source = crate::source::HprofSource::from(input);
    let p1 = pass1::Pass1::run(&source, false)?;
    if p1.class_ids.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "dump has {} objects, exceeding the {} (u32::MAX) limit of the \
                 analyzer's index scheme; cannot analyze",
                p1.class_ids.len(),
                u32::MAX
            ),
        ));
    }

    // Boolean edge-retention flags (purely query-inspection; a trivial resolver
    // suffices — see run()'s note). Escalation cannot fail on planning here since
    // the queries already planned in `parse_plan_queries`; map the error anyway.
    let run_flags = {
        let queries: Vec<query::ast::Query> = flat.iter().map(|(q, _)| q.clone()).collect();
        query::runflags::plan_run(&queries, &NoClassIndex, opts.query_path_depth).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("OQL edge planning error: {}", e.0),
            )
        })?
    };

    // No compression: dense arrays stay live so no restore dance is needed.
    // NOTE: pass2 leaves g.shallow / g.class_idx DENSE under Codec::None (it only
    // empties them when compress != None), so we read them directly below.
    let compress = cvec::Codec::None;
    let needs_sv = flat.iter().any(|(_, p)| p.needs.string_values);
    let addr_vec = if needs_sv { query::run::id_map_to_addrs(&p1.id_map) } else { Vec::new() };
    let mut no_in_sets = std::collections::HashMap::new();
    let mut no_exists_bools = std::collections::HashMap::new();
    let (
        mut g,
        inbound,
        _shallow_c,
        _class_idx_c,
        _alloc_serial_c,
        mut query_state,
        refwalk_csr,
        string_values,
        string_values_truncated,
    ) = pass2::Pass2::build(&source, p1, compress, opts, flat, &mut no_in_sets, &mut no_exists_bools)?;

    // Per-slot source-index sidecar captured during the scan (armed only when
    // `reachable_only`, via `opts.reachable_only` inside pass2). Taken BEFORE the
    // state is consumed by `resume`, so reachable-only pruning keys off the EXACT
    // source dense index rather than re-reading it from a (possibly `@objectAddress`)
    // projected row value. Empty map on `--all`.
    let row_src_by_slot = query_state.take_row_src_by_slot();

    let rpo = rpo_dfs::rpo_dfs(g.n, &g.gc_root_indices, &g.fwd_offsets, &g.fwd_targets);
    // Snapshot dfn for reachability pruning BEFORE rpo is consumed by dominators.
    let reach_dfn: Option<Vec<u32>> = if reachable_only {
        Some(rpo.dfn.clone())
    } else {
        None
    };

    // Edge-retention hook (mirrors run()): build the query-gated forward store
    // and bounded inbound CSR from the LIVE forward CSR. Under Codec::None
    // g.class_idx is dense, so borrow it in place (no restore).
    let want_forward = run_flags.retain_forward || run_flags.outbounds_by_rescan;
    let want_inbound = run_flags.retain_inbound;
    let (retained_edges, retained_inbound): RetainedEdgeStructs = if want_forward || want_inbound {
        let edge_froms: Vec<(String, bool)> = flat
            .iter()
            .filter(|(q, _)| query_uses_edges(q))
            .map(|(q, _)| (q.from.class_name().to_string(), q.from.instanceof()))
            .collect();
        let class_idx_ref: &[u32] = g.class_idx.as_slice();
        let node_matches = |s: usize| -> bool {
            let cn = &g.class_names[class_idx_ref[s] as usize];
            edge_froms
                .iter()
                .any(|(pat, _inst)| query::execute::class_name_matches(cn, pat))
        };

        let n = g.n;
        let fwd_off = &g.fwd_offsets;
        let fwd_tgt = &g.fwd_targets;

        let retained_edges = if want_forward {
            let mut builder = crate::query::retained_edges::RetainedEdgesBuilder::new();
            let mut scratch: Vec<u32> = Vec::new();
            for s in 0..n {
                if !node_matches(s) {
                    continue;
                }
                let (lo, hi) = (fwd_off[s] as usize, fwd_off[s + 1] as usize);
                fwd_tgt.copy_range(lo, hi, &mut scratch);
                scratch.sort_unstable();
                builder.push_row(s as u32, &scratch);
            }
            Some(builder.finish())
        } else {
            None
        };

        let retained_inbound = if want_inbound {
            let mut in_off = vec![0u32; n + 1];
            let mut row: Vec<u32> = Vec::new();
            for s in 0..n {
                let (lo, hi) = (fwd_off[s] as usize, fwd_off[s + 1] as usize);
                fwd_tgt.copy_range(lo, hi, &mut row);
                for &t in &row {
                    if node_matches(t as usize) {
                        in_off[t as usize + 1] += 1;
                    }
                }
            }
            for i in 0..n {
                in_off[i + 1] += in_off[i];
            }
            let total = in_off[n] as usize;
            let mut in_tgt = vec![0u32; total];
            let mut cursor = in_off.clone();
            for s in 0..n {
                let (lo, hi) = (fwd_off[s] as usize, fwd_off[s + 1] as usize);
                fwd_tgt.copy_range(lo, hi, &mut row);
                for &t in &row {
                    if node_matches(t as usize) {
                        let slot = &mut cursor[t as usize];
                        in_tgt[*slot as usize] = s as u32;
                        *slot += 1;
                    }
                }
            }
            Some((in_off, in_tgt))
        } else {
            None
        };

        (retained_edges, retained_inbound)
    } else {
        (None, None)
    };

    // Only the dominator/retained late ops actually consume the dominator tree
    // and retained-size array (`JoinRetained`/`DominatorChildren`/`DominatorOf`/
    // `RetainedSet`, surfaced as `needs.retained` / `needs.dominator_children`).
    // RefWalk, edge (`@inbounds`/`@outbounds`/path), gc-root, and string-value
    // ops escalate for their OWN structures and never read dominators. When no
    // planned query needs dominators, SKIP the inbound-transpose +
    // compute_dominators + build_dom_children_csr + compute_retained chain
    // entirely — on a large heap those dominate escalation cost. `g.idom` /
    // `g.retained` / dc_off / dc_tgt then stay empty and the LateCtx borrows
    // empty slices (the ops that would read them do not run).
    let needs_dominators = flat
        .iter()
        .any(|(_, p)| p.needs.retained || p.needs.dominator_children);

    let (dc_off, dc_tgt): (Vec<u32>, Vec<u32>) = if needs_dominators {
        // Transpose the forward CSR into the inbound CSR (consumes fwd CSR).
        let (inb_block_off, inb_data) = inbound.build_from_fwd(
            std::mem::take(&mut g.fwd_offsets),
            std::mem::take(&mut g.fwd_targets),
            &rpo.dfn,
        )?;

        // Rebuild vertex from dfn, then free dfn; parent_pre stays live (never
        // compressed under Codec::None) so compute_dominators reads it directly.
        let mut rpo = rpo;
        let count = rpo.parent_pre.len();
        rpo.vertex = rpo_dfs::rebuild_vertex(&rpo.dfn, count);
        rpo.dfn = Vec::new();

        g.idom = dominator::compute_dominators(
            g.n,
            rpo,
            &g.gc_root_indices,
            &inb_block_off,
            &inb_data,
        )?;
        drop(inb_block_off);
        drop(inb_data);

        let (dc_off, dc_tgt) = retained::build_dom_children_csr(g.n, &g.idom);

        // g.shallow / g.class_idx are dense under Codec::None — no restore needed.
        let class_count = g.class_names.len();
        let (retained, has_same, _depth_counts) = retained::compute_retained(
            g.n,
            &g.idom,
            &g.shallow,
            &g.class_idx,
            class_count,
            &g.class_obj_class_idx,
            &dc_off,
            &dc_tgt,
        );
        g.retained = retained;
        g.has_same_class_ancestor = has_same;
        (dc_off, dc_tgt)
    } else {
        // `rpo` and the forward CSR are simply dropped unused here — no dominator
        // tree, no retained sizes. Empty dc_off/dc_tgt back the (unused) LateCtx
        // dominator-children fields.
        (Vec::new(), Vec::new())
    };

    // Build the LateCtx exactly as run() does and resume the queries.
    let query_asts: Vec<query::ast::Query> = flat.iter().map(|(q, _)| q.clone()).collect();
    let empty_id_map = query::stage_runner::IdMap::new(&[]);
    let real_id_map;
    let id_map: &query::stage_runner::IdMap<'_> = if addr_vec.is_empty() {
        &empty_id_map
    } else {
        real_id_map = query::stage_runner::IdMap::new(&addr_vec);
        &real_id_map
    };
    let rw_off: &[u32] = refwalk_csr.as_ref().map_or(&[], |c| &c.fwd_off);
    let rw_tgt: &[u32] = refwalk_csr.as_ref().map_or(&[], |c| &c.fwd_tgt);
    let rw_field: &[u32] = refwalk_csr.as_ref().map_or(&[], |c| &c.fwd_field);
    let rw_names: &[String] = refwalk_csr.as_ref().map_or(&[], |c| &c.field_names);
    let rw_tails = refwalk_csr
        .as_ref()
        .map_or(&*query::stage_runner::EMPTY_REFWALK_TAILS, |c| &c.tails);
    let rw_trunc = refwalk_csr.as_ref().is_some_and(|c| c.truncated);
    let in_off: &[u32] = retained_inbound.as_ref().map_or(&[], |(o, _)| o);
    let in_tgt: &[u32] = retained_inbound.as_ref().map_or(&[], |(_, t)| t);
    let sv_ref: &std::collections::HashMap<u32, String> = if string_values.is_empty() {
        &query::stage_runner::EMPTY_STRING_VALUES
    } else {
        &string_values
    };
    let gc_root_tags: std::collections::HashMap<u32, u8> =
        if flat.iter().any(|(_, p)| p.needs.gc_roots) {
            g.gc_root_indices
                .iter()
                .zip(g.gc_root_types.iter())
                .map(|(&idx, &ty)| (idx, ty))
                .collect()
        } else {
            std::collections::HashMap::new()
        };
    let gc_root_tags_ref: &std::collections::HashMap<u32, u8> = if gc_root_tags.is_empty() {
        &query::stage_runner::EMPTY_GC_ROOT_TAGS
    } else {
        &gc_root_tags
    };
    let flat_results = query::stage_runner::resume(
        query_state,
        &query_asts,
        &query::stage_runner::LateCtx {
            retained: &g.retained,
            idom: &g.idom,
            dc_off: &dc_off,
            dc_tgt: &dc_tgt,
            shallow: &g.shallow,
            id_map: &id_map,
            fwd_off: rw_off,
            fwd_tgt: rw_tgt,
            fwd_field: rw_field,
            field_names: rw_names,
            refwalk_tails: rw_tails,
            refwalk_truncated: rw_trunc,
            in_off,
            in_tgt,
            retained_edges: retained_edges.as_ref(),
            string_values: sv_ref,
            string_values_truncated,
            gc_root_tags: gc_root_tags_ref,
        },
    );

    // Reachable-only pruning (the query-subcommand default; skipped under --all).
    // `stage_runner::resume` returns results in slot order (1:1 with `flat`), so
    // `flat_results[i]` corresponds to slot `i`. Prune each slot's rows by its
    // captured SOURCE dense index BEFORE UNION-collapse, exactly as the fast path
    // does — this handles a projected `@objectAddress` (a raw heap address) which a
    // value-sniffing prune would mis-read as a dense index and wrongly drop.
    //
    // Row-EXPANDING late ops (dominators / AS RETAINED SET / edges) emit rows that
    // are NOT the original matched objects (they are dominators / retained members /
    // referrers), so the source sidecar no longer aligns 1:1 with the output rows
    // and "was the SOURCE object reachable?" is not the right question for them.
    // Those slots are left unpruned (their captured src, if any, is skipped).
    let mut flat_results = flat_results;
    if let Some(dfn) = &reach_dfn {
        for (slot, r) in flat_results.iter_mut().enumerate() {
            let row_expanding = flat.get(slot).is_some_and(|(_, p)| {
                p.late_ops.iter().any(|op| {
                    matches!(
                        op,
                        query::plan::StageOp::RetainedSet { .. }
                            | query::plan::StageOp::DominatorChildren { .. }
                            | query::plan::StageOp::DominatorOf
                            | query::plan::StageOp::EdgeLookup { .. }
                            | query::plan::StageOp::BoundedPath { .. }
                    )
                })
            });
            if row_expanding {
                continue;
            }
            if let Some(src) = row_src_by_slot.get(&slot) {
                query::run::filter_result_by_src(r, src, dfn);
            }
        }
    }

    let results = query::run::collapse_union_results(flat_results, union_groups);

    Ok(results)
}
