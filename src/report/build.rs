//! Model builders: read the `Graph` and compute bounded aggregates into a
//! `Report` (and its sub-models). No per-object Vec is retained.

use super::*;
use crate::pass2::Graph;
use crate::pass2::{ATTRIBUTION_TOP_N, AttributionRaw};

const THRESHOLD_PCT: f64 = 10.0;
/// Default per-suspect cap on the "accumulated objects" lists (immediately
/// dominated children + by-class histogram), used as the `leak_children_cap`
/// value in unit tests. In production this is supplied by the `--detail` preset.
#[cfg(test)]
pub const DOMINATED_CAP: usize = 50;
/// MAT `FindLeaksQuery.big_drop_ratio`: descend the dominator tree while the
/// largest child retains at least this fraction of its parent; stop (parent is
/// the accumulation point) on the first drop below it.
const BIG_DROP_RATIO: f64 = 0.7;
/// MAT `FindLeaksQuery.MAX_DEPTH`: give up the accumulation-point descent after
/// this many steps without a big drop (no accumulation point reported).
const MAX_ACCUM_DEPTH: usize = 1000;
/// MAT 1%-of-total pruning threshold for the package tree, in basis points.
const PACKAGE_THRESHOLD_BP: u32 = 100;
/// Cap on the number of rows in the per-class unreachable-objects histogram
/// (top classes by shallow). Additive section; not parity-gated.
pub(crate) const UNREACHABLE_HISTOGRAM_CAP: usize = 30;
/// Cap on the number of rows in the "Big Drops" dominator view.
const BIG_DROPS_CAP: usize = 25;
/// Cap on the number of rows in the "Immediate Dominators" class rollup.
const IMMEDIATE_DOMINATORS_CAP: usize = 30;
/// Cap on the number of (dominator_class, dominated_class) pairs emitted for
/// the V5 two-sided sankey. 20k covers ~10 dominator pairs per class for heaps
/// with up to ~2000 significant classes, enabling the full pivot navigation.
const IMDOM_PAIRS_CAP: usize = 20_000;
/// Cap on the TOTAL number of nodes emitted across a group suspect's merged
/// shortest-paths-to-GC-roots prefix tree. Once reached, existing matching
/// nodes keep accumulating counts/retained (so totals stay meaningful) but no
/// new branches are created — deterministic, RSS-bounded.
const MERGED_PATH_MAX_NODES: usize = 60;

// ── Model construction ───────────────────────────────────────────────────────

/// Build the flat object graph lookup table for V3/V4 (Reference Graph Explorer
/// and Dominator Tree Explorer). Walks the dominator tree from vroot BFS,
/// collecting nodes with retained >= sig_floor, and populates edges from the
/// `ObjGraphCapture` snapshot taken earlier.
fn build_obj_graph_flat(
    g: &Graph,
    dc_offsets: &[u32],
    dc_targets: &[u32],
    edge_cap: usize,
    size_tier: &str,
) -> ObjGraphFlat {
    use std::collections::{HashMap, HashSet, VecDeque};
    let n = g.n;
    let vroot = n as u32;
    let total_shallow: u64 = g.shallow.iter().map(|&s| s as u64).sum();
    // Show objects retaining >= 0.1% of total heap, but at least 64 KB so tiny
    // heaps (test fixtures) still produce visible nodes.
    let sig_floor: u64 = (total_shallow / 1000).max(65_536);
    let root_floor: u64 = (total_shallow / 100).max(1_048_576);

    let dom_children_of = |node: u32| -> &[u32] {
        let idx = node as usize;
        if idx + 1 < dc_offsets.len() {
            &dc_targets[dc_offsets[idx] as usize..dc_offsets[idx + 1] as usize]
        } else {
            &[]
        }
    };

    // BFS from vroot children to collect significant nodes
    let mut nodes: HashMap<u32, ObjGraphFlatNode> = HashMap::new();
    let mut dom_children_map: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut roots: Vec<u32> = Vec::new();

    // Find direct children of vroot with retained >= root_floor
    let vroot_usize = vroot as usize;
    let vroot_children: Vec<u32> = if vroot_usize + 1 < dc_offsets.len() {
        dc_targets[dc_offsets[vroot_usize] as usize..dc_offsets[vroot_usize + 1] as usize].to_vec()
    } else {
        Vec::new()
    };

    let mut queue: VecDeque<u32> = VecDeque::new();
    for &root in &vroot_children {
        if (root as usize) < g.retained.len() && g.retained[root as usize] >= root_floor {
            roots.push(root);
            queue.push_back(root);
        }
    }
    roots.sort_unstable_by(|&a, &b| g.retained[b as usize].cmp(&g.retained[a as usize]));

    let mut visited: HashSet<u32> = HashSet::new();
    while let Some(node) = queue.pop_front() {
        if !visited.insert(node) {
            continue;
        }
        let idx = node as usize;
        if idx >= g.retained.len() {
            continue;
        }
        let ret = g.retained[idx];
        if ret < sig_floor {
            continue;
        }

        let ci = g.class_idx[idx] as usize;
        let class_name = if ci < g.class_names.len() {
            pretty_class_name(&g.class_names[ci])
        } else {
            format!("obj#{}", node)
        };

        let idom = if g.idom[idx] == vroot {
            None
        } else {
            Some(g.idom[idx])
        };

        nodes.insert(
            node,
            ObjGraphFlatNode {
                display_class: class_name,
                shallow: g.shallow[idx] as u64,
                retained: ret,
                edges_unknown: false, // set below
                edges_truncated: false,
                idom,
                dom_subtree_count: 0,        // computed below
                subtree_classes: Vec::new(), // computed below
            },
        );

        let kids = dom_children_of(node);
        let sig_kids: Vec<u32> = kids
            .iter()
            .copied()
            .filter(|&k| (k as usize) < g.retained.len() && g.retained[k as usize] >= sig_floor)
            .collect();
        if !sig_kids.is_empty() {
            dom_children_map.insert(node, sig_kids.clone());
            for k in sig_kids {
                queue.push_back(k);
            }
        }
    }

    // Bottom-up subtree count via iterative postorder DFS (avoids stack overflow).
    {
        let mut subtree_counts: HashMap<u32, u32> = HashMap::new();
        for &root in &roots {
            let mut stack: Vec<(u32, usize)> = vec![(root, 0)];
            while let Some((node, child_idx)) = stack.last_mut() {
                let node = *node;
                let children = dom_children_map
                    .get(&node)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                if *child_idx < children.len() {
                    let child = children[*child_idx];
                    *child_idx += 1;
                    if !subtree_counts.contains_key(&child) {
                        stack.push((child, 0));
                    }
                } else {
                    stack.pop();
                    let child_sum: u32 = dom_children_map
                        .get(&node)
                        .map(|kids| {
                            kids.iter()
                                .map(|k| *subtree_counts.get(k).unwrap_or(&1))
                                .sum()
                        })
                        .unwrap_or(0);
                    subtree_counts.insert(node, 1 + child_sum);
                }
            }
        }
        for (&node_id, node) in nodes.iter_mut() {
            node.dom_subtree_count = *subtree_counts.get(&node_id).unwrap_or(&1);
        }
    }

    // Subtree class histogram: for each significant node, store the top-10 classes
    // by shallow heap in its dominated subtree.
    //
    // Algorithm: forward BFS from vroot to build nearest_sig[i] = the nearest
    // significant ancestor of i (or u32::MAX). Then single scan over all objects:
    // add each object's (class, shallow) to its nearest significant ancestor's
    // histogram. O(n) time, one u32 per object (160 MB at n=40M).
    {
        const TOP_K: usize = 10;
        const NO_SIG: u32 = u32::MAX;

        // BFS from vroot level-by-level to assign nearest_sig.
        let mut nearest_sig = vec![NO_SIG; n + 1]; // +1 for vroot slot
        // vroot itself has no significant parent.
        nearest_sig[n] = NO_SIG;

        // BFS queue: process nodes in BFS order so parent nearest_sig is set
        // before children.
        let mut bfs: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
        // Start from vroot's children.
        let vroot_usize = vroot as usize;
        if vroot_usize + 1 < dc_offsets.len() {
            for &child in
                &dc_targets[dc_offsets[vroot_usize] as usize..dc_offsets[vroot_usize + 1] as usize]
            {
                bfs.push_back(child);
            }
        }
        while let Some(node) = bfs.pop_front() {
            let node_usize = node as usize;
            // Determine nearest_sig for this node.
            let parent_sig = if node_usize + 1 < dc_offsets.len() {
                // Find parent via idom (g.idom[node] is the immediate dominator).
                let par = if node_usize < g.idom.len() {
                    g.idom[node_usize]
                } else {
                    vroot
                };
                if par as usize <= n {
                    nearest_sig[par as usize]
                } else {
                    NO_SIG
                }
            } else {
                NO_SIG
            };
            nearest_sig[node_usize] = if nodes.contains_key(&node) {
                node
            } else {
                parent_sig
            };
            // Enqueue children.
            if node_usize + 1 < dc_offsets.len() {
                for &child in &dc_targets
                    [dc_offsets[node_usize] as usize..dc_offsets[node_usize + 1] as usize]
                {
                    bfs.push_back(child);
                }
            }
        }

        // Single pass: accumulate (class, shallow) for each object into its
        // nearest significant ancestor's histogram.
        type ClassMap = std::collections::HashMap<u32, (u32, u64)>;
        let mut histograms: HashMap<u32, ClassMap> = HashMap::with_capacity(nodes.len());
        for i in 0..n {
            let sig = nearest_sig[i];
            if sig == NO_SIG {
                continue;
            }
            let ci = g.class_idx.get(i).copied().unwrap_or(0);
            let sh = g.shallow.get(i).copied().unwrap_or(0) as u64;
            let hm = histograms.entry(sig).or_default();
            let e = hm.entry(ci).or_insert((0, 0));
            e.0 += 1;
            e.1 += sh;
        }
        drop(nearest_sig);

        // Extract top-K for each significant node.
        for (&node, node_entry) in nodes.iter_mut() {
            if let Some(hm) = histograms.remove(&node) {
                let mut rows: Vec<(u32, u32, u64)> =
                    hm.iter().map(|(&ci, &(cnt, sh))| (ci, cnt, sh)).collect();
                rows.sort_unstable_by_key(|r| std::cmp::Reverse(r.2));
                rows.truncate(TOP_K);
                node_entry.subtree_classes = rows
                    .into_iter()
                    .map(|(ci, cnt, sh)| {
                        let name = if (ci as usize) < g.class_names.len() {
                            pretty_class_name(&g.class_names[ci as usize])
                        } else {
                            format!("obj#{}", ci)
                        };
                        SubtreeClassRow {
                            class: name,
                            instance_count: cnt,
                            total_shallow: sh,
                        }
                    })
                    .collect();
            }
        }
    }

    // Populate edges from ObjGraphCapture
    let mut edges_map: HashMap<u32, Vec<ObjGraphEdge>> = HashMap::new();
    if let Some(cap) = g.obj_graph_edges.as_ref() {
        for &src in nodes.keys().copied().collect::<Vec<_>>().iter() {
            let raw_edges = cap.edges_of(src as usize);
            if raw_edges.is_empty() {
                continue;
            }
            let truncated = raw_edges.len() > 100;
            let mut out: Vec<ObjGraphEdge> = Vec::with_capacity(raw_edges.len().min(100));
            for &(dst, name_idx) in raw_edges.iter().take(100) {
                let field_name = if name_idx == 0 {
                    String::new()
                } else {
                    cap.field_name_pool
                        .get(name_idx as usize)
                        .cloned()
                        .unwrap_or_default()
                };
                let dst_ci = g.class_idx.get(dst as usize).copied().unwrap_or(0) as usize;
                let child_class = if dst_ci < g.class_names.len() {
                    pretty_class_name(&g.class_names[dst_ci])
                } else {
                    format!("obj#{}", dst)
                };
                let child_retained = g.retained.get(dst as usize).copied().unwrap_or(0);
                out.push(ObjGraphEdge {
                    field_name,
                    child_idx: dst,
                    child_class,
                    child_retained,
                });
            }
            if let Some(node) = nodes.get_mut(&src) {
                node.edges_truncated = truncated;
            }
            if !out.is_empty() {
                edges_map.insert(src, out);
            }
        }
        // Mark nodes not in capture set as edges_unknown
        for (&idx, node) in nodes.iter_mut() {
            if !cap.captured.get(idx as usize) {
                node.edges_unknown = true;
            }
        }
    } else {
        // No capture: all nodes have edges_unknown
        for node in nodes.values_mut() {
            node.edges_unknown = true;
        }
    }

    // Populate inbound_edges from ObjGraphCapture.inbound
    let mut inbound_map: std::collections::HashMap<u32, Vec<InboundEdge>> =
        std::collections::HashMap::new();
    let mut inbound_truncated_set: std::collections::HashSet<u32> =
        std::collections::HashSet::new();
    if let Some(cap) = g.obj_graph_edges.as_ref() {
        for &dst in nodes.keys().copied().collect::<Vec<_>>().iter() {
            let raw_inbound = cap.inbound_of(dst as usize);
            if raw_inbound.is_empty() {
                continue;
            }
            let inbound_edges: Vec<InboundEdge> = raw_inbound
                .iter()
                .map(|&(src_idx, name_idx)| {
                    let field_name = if name_idx == 0 {
                        String::new()
                    } else {
                        cap.field_name_pool
                            .get(name_idx as usize)
                            .cloned()
                            .unwrap_or_default()
                    };
                    let src_usize = src_idx as usize;
                    let src_ci = g.class_idx.get(src_usize).copied().unwrap_or(0) as usize;
                    let src_class = if src_ci < g.class_names.len() {
                        pretty_class_name(&g.class_names[src_ci])
                    } else {
                        format!("obj#{}", src_idx)
                    };
                    let src_shallow = g.shallow.get(src_usize).copied().unwrap_or(0) as u64;
                    let src_retained = g.retained.get(src_usize).copied().unwrap_or(0);
                    InboundEdge {
                        src_idx,
                        field_name,
                        src_class,
                        src_shallow,
                        src_retained,
                    }
                })
                .collect();
            if !inbound_edges.is_empty() {
                inbound_map.insert(dst, inbound_edges);
            }
            if cap.inbound_truncated.get(dst as usize) {
                inbound_truncated_set.insert(dst);
            }
        }
    }

    // Pre-build depth-3 DomTreeNode trees for top-20 roots
    let display_of = |i: usize| -> String {
        let ci = g.class_idx[i] as usize;
        if ci < g.class_names.len() {
            pretty_class_name(&g.class_names[ci])
        } else {
            format!("obj#{}", i)
        }
    };
    let top_roots: Vec<u32> = roots.iter().take(20).copied().collect();
    let root_dom_trees: Vec<(u32, DomTreeNode)> = top_roots
        .iter()
        .map(|&root| {
            let tree = build_dom_subtree(
                root as usize,
                dc_offsets,
                dc_targets,
                &display_of,
                g,
                200, // max_nodes
                3,   // max_depth
            );
            (root, tree)
        })
        .collect();

    ObjGraphFlat {
        nodes,
        edges: edges_map,
        dom_children: dom_children_map,
        root_dom_trees,
        roots,
        sig_floor_bytes: sig_floor,
        inbound_edges: inbound_map,
        inbound_truncated: inbound_truncated_set,
        capture_params: CaptureParams {
            edge_cap,
            size_tier: size_tier.to_string(),
        },
    }
}

/// Build the Type-Level Reference Graph (TPFG, V13).
/// Uses the pre-aggregated class-pair count map (`g.type_ref_pairs`) that was
/// built from the live fwd-CSR before inbound construction consumed it.
/// Retained weight is estimated as: edge_count × avg_retained_per_src_class.
fn build_type_ref_graph(g: &Graph) -> Vec<TypeEdge> {
    let pairs = match g.type_ref_pairs.as_ref() {
        Some(p) => p,
        None => return vec![],
    };

    // Build per-class total retained and count so we can estimate retained weight.
    // Only classes that appear as sources in the pair map need this.
    let mut class_total_retained: Vec<u64> = vec![0u64; g.class_names.len()];
    let mut class_instance_count: Vec<u64> = vec![0u64; g.class_names.len()];
    for (i, &ci) in g.class_idx.iter().enumerate() {
        let ci = ci as usize;
        if ci < class_total_retained.len() {
            class_total_retained[ci] += g.retained.get(i).copied().unwrap_or(0);
            class_instance_count[ci] += 1;
        }
    }

    let pair_fields = g.type_ref_pair_fields.as_ref();

    let mut edges: Vec<TypeEdge> = pairs
        .iter()
        .map(|(&(sci, dci), &edge_count)| {
            let src_class = if (sci as usize) < g.class_names.len() {
                crate::report::pretty_class_name(&g.class_names[sci as usize])
            } else {
                format!("cls#{}", sci)
            };
            let dst_class = if (dci as usize) < g.class_names.len() {
                crate::report::pretty_class_name(&g.class_names[dci as usize])
            } else {
                format!("cls#{}", dci)
            };
            let sci_usize = sci as usize;
            let avg_retained =
                if sci_usize < class_instance_count.len() && class_instance_count[sci_usize] > 0 {
                    class_total_retained[sci_usize] / class_instance_count[sci_usize]
                } else {
                    0
                };
            let retained_weight = edge_count.saturating_mul(avg_retained);
            let top_field_names = pair_fields
                .and_then(|m| m.get(&(sci, dci)))
                .map(|v| v.iter().map(|(n, _)| n.clone()).collect())
                .unwrap_or_default();
            TypeEdge {
                src_class,
                dst_class,
                edge_count,
                retained_weight,
                top_field_names,
            }
        })
        .collect();

    edges.sort_unstable_by_key(|e: &TypeEdge| std::cmp::Reverse(e.retained_weight));
    edges.truncate(500);
    edges
}

/// Compute all report aggregates from the graph.
///
/// Ordering mirrors the previous three separate render calls so callers keep
/// the same free-as-you-go RSS discipline: the system-overview group is
/// computed first (the only reader of `has_same_class_ancestor`), then the
/// leak-suspect group (the only reader of `dc_offsets`/`dc_targets`), then top
/// consumers. Takes `dc_offsets` and `dc_targets` by value so it can free them
/// (~4 GB on large dumps) immediately after their last use (before the remaining
/// collection/attribution/framework sections allocate intermediate data).
/// Takes `g` as `&mut Graph` to allow freeing `g.idom` (~2 GB) after
/// `build_references`, the last consumer of it.
pub fn build_model(
    g: &mut Graph,
    dc_offsets: Vec<u32>,
    dc_targets: Vec<u32>,
    leak_children_cap: usize,
    depth_counts: &[u64],
    opts: &crate::AnalyzeOptions,
    alloc_sites: Option<AllocSites>,
    precomputed_field_stats: Option<FieldStats>,
) -> Report {
    let generated = now_iso8601();
    // Per-step wall markers to attribute build_model's ~157s (the biggest touchable
    // serial WALL lever in the dark phase). Relative to build_model entry. Gated on
    // HPROF_TIMING like the pass2/t_dark markers; stderr-only, byte-exact.
    #[cfg(not(target_family = "wasm"))]
    let _t_bm = std::time::Instant::now();
    macro_rules! t_bm {
        ($label:expr) => {
            #[cfg(not(target_family = "wasm"))]
            if std::env::var_os("HPROF_TIMING").is_some() {
                eprintln!(
                    "[timing] build_model/{}: {:.3}s",
                    $label,
                    _t_bm.elapsed().as_secs_f64()
                );
            }
        };
    }
    // Call the dc_offsets/dc_targets consumers FIRST so we can drop those ~4 GB
    // arrays before build_system_overview. build_system_overview only needs
    // idom/retained/shallow/class_idx — not the dominator-children CSR — so
    // running it after the dc drop cuts ~4 GB from the build_model RSS peak.
    // (build_stack_held_via and build_top_consumers also don't need dc, so they
    // can run here too, before overview is available for thread/top_components.)
    let leaks = build_leak_suspects(
        g,
        &dc_offsets,
        &dc_targets,
        leak_children_cap,
        opts.root_path_max_depth,
        opts.dominator_tree_max_nodes,
        opts.dominator_tree_max_depth,
    );
    t_bm!("leak_suspects");
    crate::trace::probe("build_model: after leak_suspects aggregates");
    let dominator_analysis =
        build_dominator_analysis(g, &dc_offsets, &dc_targets, depth_counts.len() as u32);
    t_bm!("dominator_analysis");
    crate::trace::probe("build_model: after dominator_analysis aggregates");
    let obj_graph_flat = if opts.obj_graph {
        Some(build_obj_graph_flat(
            g,
            &dc_offsets,
            &dc_targets,
            opts.report_size.edge_cap(),
            opts.report_size.tier_name(),
        ))
    } else {
        None
    };
    t_bm!("obj_graph_flat");
    // dc_offsets and dc_targets are not used after this point. Drop them now
    // (~4 GB on large dumps) before build_system_overview, so the ~4 GB dc
    // arrays are off the RSS budget during that O(n) scan.
    drop(dc_offsets);
    drop(dc_targets);
    crate::trace::trim();
    crate::trace::probe("build_model: after drop(dc) — before system_overview");
    crate::trace::probe("build_model: before system_overview aggregates");
    let (overview, top_level_list) =
        build_system_overview(g, depth_counts, opts.top_consumers, opts.hist_root_path_top);
    t_bm!("system_overview");
    crate::trace::probe("build_model: after system_overview aggregates");
    let stack_held_via = build_stack_held_via(g);
    let top = build_top_consumers(g, opts.top_consumers, &stack_held_via, top_level_list);
    t_bm!("top_consumers");
    let threads = build_thread_overview(g, overview.total_shallow);
    t_bm!("thread_overview");
    crate::trace::probe("build_model: after thread_overview aggregates");
    let top_components = build_top_components(&overview);
    t_bm!("top_components");
    crate::trace::probe("build_model: after top_components aggregates");
    crate::trace::probe("build_model: after drop(dc+cap) — before type_ref/references/collections");
    let type_ref_graph = if opts.obj_graph {
        build_type_ref_graph(g)
    } else {
        vec![]
    };
    t_bm!("type_ref_graph");
    // obj_graph_edges (sparse capture) is not used after type_ref_graph. Free it now.
    drop(g.obj_graph_edges.take());
    // type_ref_pairs (pre-aggregated class-pair counts) is consumed by build_type_ref_graph.
    drop(g.type_ref_pairs.take());
    // type_ref_pair_fields (field-name tallies for type edges) is consumed by build_type_ref_graph.
    drop(g.type_ref_pair_fields.take());
    crate::trace::trim();
    let references = build_references(g);
    t_bm!("references");
    crate::trace::probe("build_model: after references only-weakly-retained rollup");
    // g.idom is not used after build_references. Free it now (~2 GB on large dumps)
    // before collection/attribution/framework sections run.
    crate::trace::drop_vec(std::mem::take(&mut g.idom));
    crate::trace::trim();
    crate::trace::probe("build_model: after drop(g.idom) — before collection_attribution");
    let collection_attribution = build_collection_attribution(g, &overview);
    t_bm!("collection_attribution");
    crate::trace::probe("build_model: after collection_attribution");
    let fields_by_size = build_fields_by_size(g, &overview);
    t_bm!("fields_by_size");
    crate::trace::probe("build_model: after fields_by_size");
    let biggest_collections = build_biggest_collections(g);
    let collection_contents = build_collection_contents(g);
    t_bm!("biggest_collections+contents");
    crate::trace::probe("build_model: after biggest_collections+contents");
    let top_retainers = build_top_retainers(&fields_by_size, &threads);
    t_bm!("top_retainers");
    crate::trace::probe("build_model: after top_retainers");
    // ThreadLocal Leak Analyzer — gated on find_duplicates (same opt-in as dup
    // analysis; also implicitly enabled by --full-analysis via the flag fold).
    let thread_local_analysis = if opts.find_duplicates {
        build_threadlocal_analysis(g)
    } else {
        Vec::new()
    };
    t_bm!("thread_local_analysis");
    crate::trace::probe("build_model: after thread_local_analysis");
    // Framework Auto-Analysis — always-on; each framework only emits when its
    // sentinel class is present in the heap.
    let framework_analysis = crate::pass2::scan_frameworks(g);
    t_bm!("framework_analysis");
    crate::trace::probe("build_model: after framework_analysis");
    let field_stats = if precomputed_field_stats.is_some() {
        // Precomputed before build_model to free the fwd CSR copy early (saves ~2 GB peak RSS).
        precomputed_field_stats
    } else if opts.field_stats {
        let fs = Some(build_field_stats(g));
        // Free the restored fwd CSR (only alive when --field-stats was passed).
        g.fwd_offsets = Vec::new();
        g.fwd_targets = crate::chunkvec::ChunkU32::zeroed(0);
        fs
    } else {
        None
    };
    t_bm!("field_stats");
    let mut report = Report {
        schema_version: SCHEMA_VERSION,
        generated,
        truncated_input: false,
        overview,
        leaks,
        top,
        threads,
        top_components,
        alloc_sites,
        arrays_by_size: g.arrays_by_size.clone(),
        dominator_analysis,
        collections: g.collections.clone(),
        references,
        collection_attribution,
        fields_by_size,
        biggest_collections,
        collection_contents,
        leak_indicators: build_leak_indicators(g),
        waste_summary: None,
        triage: Vec::new(),
        top_retainers,
        queries: Vec::new(),
        analysis_flags: crate::report::AnalysisFlags {
            find_duplicates: opts.find_duplicates,
            collections: opts.collections,
            obj_graph: opts.obj_graph,
            ref_paths: opts.ref_paths,
        },
        obj_graph_flat,
        type_ref_graph,
        thread_local_analysis,
        framework_analysis,
        field_stats,
    };
    // Fold every quantifiable waste source into one headline reclaimable figure.
    report.waste_summary = build_waste_summary(&report);
    // Evaluate the OOM-triage rule framework once over the finished report.
    report.triage = crate::report::evaluate_triage(&report);
    t_bm!("waste_summary+triage+done");
    // Invariant: the "% Heap" denominator is one number. `leaks.total_shallow`
    // and `overview.total_shallow` are computed by separate passes but must agree,
    // or the same figure would slug to different percentages in different sections.
    debug_assert_eq!(
        report.leaks.total_shallow, report.overview.total_shallow,
        "reachable-shallow basis diverged: leaks={} overview={}",
        report.leaks.total_shallow, report.overview.total_shallow,
    );
    report
}

fn is_anonymous_class(name: &str) -> bool {
    // $<digits-only> — anonymous inner class (e.g. Foo$1, Bar$23)
    if let Some(pos) = name.rfind('$') {
        let after = &name[pos + 1..];
        if !after.is_empty() && after.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    // Lambda, cglib anon, and reflection proxy patterns
    name.contains("$$Lambda$") || name.contains("$$Anon") || name.contains("$Proxy")
}

/// Fold every quantifiable waste source into one headline "reclaimable N bytes"
/// figure. Sources are approximate and may overlap slightly; `total_bytes` is
/// their arithmetic sum (the plan's §24 headline). Returns `None` when every
/// source is zero so the section is omitted rather than showing "0 B".
///
/// Reads only already-computed aggregates on the finished `Report`, so it runs
/// after the rest of the model is built and adds no heap pass.
fn build_waste_summary(report: &Report) -> Option<WasteSummary> {
    let mut sources: Vec<WasteSource> = Vec::new();
    let mut push = |label: &str, bytes: u64, anchor: Option<&str>| {
        if bytes > 0 {
            sources.push(WasteSource {
                label: label.to_string(),
                bytes,
                anchor: anchor.map(|s| s.to_string()),
            });
        }
    };

    // Under-filled collections: (capacity − used) × slot width, already summed
    // into each fill-ratio bucket's `wasted`.
    let coll_fill: u64 = report
        .collections
        .collection_fill_ratio
        .buckets
        .iter()
        .map(|b| b.wasted)
        .sum();
    push(
        "Under-filled Collections",
        coll_fill,
        Some(SectionId::Collections.slug()),
    );

    // Under-filled object arrays: null slots × reference width.
    let arr_fill: u64 = report
        .collections
        .array_fill_ratio
        .buckets
        .iter()
        .map(|b| b.wasted)
        .sum();
    push(
        "Under-filled Object Arrays",
        arr_fill,
        Some(SectionId::Collections.slug()),
    );

    if let Some(dup) = report.overview.duplicate_strings.as_ref() {
        push(
            "Duplicate String Values",
            dup.approx_wasted_bytes,
            Some(SectionId::DuplicateStrings.slug()),
        );
        if let Some(caw) = dup.char_array_waste.as_ref() {
            push(
                "String Backing-Array Slack",
                caw.total_wasted_bytes,
                Some(SectionId::DuplicateStrings.slug()),
            );
        }
    }

    if let Some(dpa) = report.overview.duplicate_prim_arrays.as_ref() {
        // Rendered as a `###` subsection of System Overview, no dedicated anchor.
        push("Duplicate Primitive Arrays", dpa.total_wasted_bytes, None);
    }

    if sources.is_empty() {
        return None;
    }
    // Largest reclaimable source first; ties broken by label for stable output.
    sources.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.label.cmp(&b.label)));
    let total_bytes: u64 = sources.iter().map(|s| s.bytes).sum();
    // Proportional estimate of waste in reachable objects: scale total_bytes by
    // the reachable fraction of the whole heap. Approximate — waste scans do not
    // separate reachable from unreachable per-instance.
    let reachable = report.overview.total_shallow;
    let total_heap = reachable + report.overview.unreachable_shallow;
    let reachable_bytes = if total_heap > 0 {
        (total_bytes as u128 * reachable as u128 / total_heap as u128) as u64
    } else {
        total_bytes
    };
    Some(WasteSummary {
        total_bytes,
        reachable_bytes,
        sources,
    })
}

fn build_leak_indicators(g: &Graph) -> LeakIndicators {
    // 1. Anonymous/generated class count — one entry per distinct class in class_names.
    let anonymous_class_count = g
        .class_names
        .iter()
        .filter(|n| is_anonymous_class(n))
        .count() as u64;

    // 2. ThreadLocalMap$Entry null-key count — computed during the pass2
    // field-decode scan, where the weak referent's nullness is directly
    // observable. It cannot be reconstructed here from the forward CSR: a null
    // referent is an absent edge, indistinguishable from any other missing
    // target, and every instance additionally carries an untagged class-object
    // forward edge that would defeat an "any live target" heuristic.
    let thread_local_null_key_count = g.thread_local_null_key_count;

    // 3. DirectByteBuffer capacity sum — already computed in pass2.
    let direct_byte_buffer_capacity_sum = g.direct_byte_buffer_capacity_sum;

    LeakIndicators {
        anonymous_class_count,
        thread_local_null_key_count,
        direct_byte_buffer_capacity_sum,
    }
}

/// Build the ThreadLocal Leak Analyzer breakdown. Iterates `g.tl_entry_records`
/// (captured at field-decode time), looks up each value object's class name and
/// retained heap, then aggregates by class. Rows are sorted by retained heap
/// descending, then entry_count descending, then value_class ascending.
///
/// Only called when `--find-duplicates` / `--full-analysis` is set.
fn build_threadlocal_analysis(g: &Graph) -> Vec<ThreadLocalLeakRow> {
    use std::collections::HashMap;
    if g.tl_entry_records.is_empty() {
        return Vec::new();
    }

    // key: value_class name → (entry_count, stale_count, retained_sum)
    let mut by_class: HashMap<String, (u32, u32, u64)> = HashMap::new();

    for &(is_stale, val_idx) in &g.tl_entry_records {
        let val_idx = val_idx as usize;
        let class_name = if val_idx == usize::MAX {
            // null value — attribute to a synthetic placeholder
            "<null value>".to_string()
        } else {
            class_display(g, val_idx)
        };
        let retained = if val_idx != usize::MAX && val_idx < g.retained.len() {
            g.retained[val_idx]
        } else {
            0
        };
        let e = by_class.entry(class_name).or_insert((0, 0, 0));
        e.0 += 1; // entry_count
        if is_stale {
            e.1 += 1; // stale_count
        }
        e.2 += retained;
    }

    let mut rows: Vec<ThreadLocalLeakRow> = by_class
        .into_iter()
        .map(
            |(value_class, (entry_count, stale_count, retained))| ThreadLocalLeakRow {
                value_class,
                entry_count,
                stale_count,
                retained,
            },
        )
        .collect();

    // Sort: retained desc, entry_count desc, value_class asc for determinism.
    rows.sort_unstable_by(|a, b| {
        b.retained
            .cmp(&a.retained)
            .then_with(|| b.entry_count.cmp(&a.entry_count))
            .then_with(|| a.value_class.cmp(&b.value_class))
    });

    rows
}

/// Pretty class-display name for object index `i`, matching the derivation used
/// throughout the report (`build_dominator_analysis`'s `display_of`): resolve
/// the object's class row and render it via `pretty_class_name`. Returns an
/// empty string when the object index `i` or the resolved class row is out of
/// range.
fn class_display(g: &Graph, i: usize) -> String {
    let Some(&raw_ci) = g.class_idx.get(i) else {
        return String::new();
    };
    let ci = raw_ci as usize;
    if ci < g.class_names.len() {
        pretty_class_name(&g.class_names[ci])
    } else {
        String::new()
    }
}

/// Resolve the display type string for a collection element at dense index
/// `vi`. For plain elements, returns the class name. For Node/Entry wrapper
/// objects whose key/value fields were recorded in `g.node_kv`, returns
/// `"KeyClass → ValueClass"` (or a partial form when only one side is
/// available). Falls back to the raw class name when no KV data is present.
fn element_type_display(g: &Graph, vi: u32) -> String {
    if let Some(kv_map) = &g.node_kv {
        if let Some(&(key_idx, val_idx)) = kv_map.get(&vi) {
            let key_cls = if key_idx != u32::MAX {
                class_display(g, key_idx as usize)
            } else {
                String::new()
            };
            let val_cls = if val_idx != u32::MAX {
                class_display(g, val_idx as usize)
            } else {
                String::new()
            };
            return match (key_cls.is_empty(), val_cls.is_empty()) {
                (false, false) => format!("{} \u{2192} {}", key_cls, val_cls),
                (false, true) => key_cls,
                (true, false) => val_cls,
                (true, true) => class_display(g, vi as usize),
            };
        }
    }
    class_display(g, vi as usize)
}

/// Build the reference-kind statistics for the report from the graph's
/// always-on reference analysis, filling in each present kind's
/// `only_weakly_retained` rollup.
///
/// A referent is "only weakly retained" iff it has NO strong dominator: because
/// the `referent` edge is excluded from the dominator tree, `g.idom[i] == undef`
/// means the object is reachable ONLY through the weak/soft/phantom edge. Those
/// referents are grouped by class (objects counted, shallow summed) from the
/// per-kind capped referent-index lists. RSS-neutral: the only new allocation is
/// a `HashMap<String,(u64,u64)>` bounded by the number of distinct referent
/// classes per kind.
fn build_references(g: &Graph) -> ReferencesAnalysis {
    use std::collections::HashMap;
    let undef = u32::MAX;
    let mut references = g.references.clone();

    // (Option<ReferenceStats>, referent-index list) per kind: 0=Soft,1=Weak,2=Phantom.
    let mut per_kind: [&mut Option<ReferenceStats>; 3] = [
        &mut references.soft,
        &mut references.weak,
        &mut references.phantom,
    ];
    for (kind, stats) in per_kind.iter_mut().enumerate() {
        let Some(stats) = stats.as_mut() else {
            continue;
        };

        // Count null referents: tracked at scan time in fielddecode.rs.
        stats.null_referent_count = g.reference_null_referent_count[kind];

        // Accumulate retained per class for the referent histogram (capped at
        // REFERENT_HIST_CAP classes; overflow lands in "<other>"). The
        // HashMap is bounded to the same 200-entry cap as the histogram itself.
        // This is a no-alloc pass: HashMap keys are borrowed from the existing
        // histogram rows, so we build a lookup map from row index.
        let known_classes: HashMap<&str, usize> = stats
            .referent_histogram
            .iter()
            .enumerate()
            .map(|(i, r)| (r.pretty_class.as_str(), i))
            .collect();
        let mut retained_per_class = vec![0u64; stats.referent_histogram.len()];
        let mut retained_other = 0u64;
        for &ri in &g.reference_referent_idx[kind] {
            let i = ri as usize;
            let ret = if i < g.retained.len() {
                g.retained[i]
            } else {
                0
            };
            let cls = class_display(g, i);
            if let Some(&idx) = known_classes.get(cls.as_str()) {
                retained_per_class[idx] += ret;
            } else {
                retained_other += ret;
            }
        }
        for (row, &ret) in stats
            .referent_histogram
            .iter_mut()
            .zip(retained_per_class.iter())
        {
            row.retained = ret;
        }
        // Back-fill the "<other>" row if present (always last when non-empty).
        if let Some(other_row) = stats
            .referent_histogram
            .last_mut()
            .filter(|r| r.pretty_class == "<other>")
        {
            other_row.retained = retained_other;
        }

        // only_weakly_retained: referents with no strong dominator (idom == undef).
        let mut by_class: HashMap<String, (u64, u64, u64)> = HashMap::new();
        for &ri in &g.reference_referent_idx[kind] {
            let i = ri as usize;
            if g.idom[i] != undef {
                continue; // has a strong dominator -> not only-weakly-retained
            }
            let e = by_class.entry(class_display(g, i)).or_insert((0, 0, 0));
            e.0 += 1;
            e.1 += g.shallow[i] as u64;
            e.2 += if i < g.retained.len() {
                g.retained[i]
            } else {
                0
            };
        }
        let mut rows: Vec<RefStatClassRow> = by_class
            .into_iter()
            .map(
                |(pretty_class, (objects, shallow, retained))| RefStatClassRow {
                    pretty_class,
                    objects,
                    shallow,
                    retained,
                },
            )
            .collect();
        // Deterministic: retained desc, then pretty_class asc.
        rows.sort_unstable_by(|a, b| {
            b.retained
                .cmp(&a.retained)
                .then_with(|| a.pretty_class.cmp(&b.pretty_class))
        });
        stats.only_weakly_retained = rows;
    }

    references
}

/// Kind label for a raw record's `container_kind` byte. `_` maps to "mixed",
/// used both for unexpected bytes and as the aggregated label when one
/// `(holder,field)` key spans containers of more than one kind.
fn kind_label(k: u8) -> &'static str {
    match k {
        0 => "list",
        1 => "map",
        2 => "set",
        3 => "deque",
        4 => "queue",
        5 => "tree",
        6 => "object array",
        7 => "primitive array",
        _ => "mixed",
    }
}

/// Build the container-attribution rankings from the raw field-decode records,
/// attaching each container's retained size via its dense index. `None` when
/// `--collections` was off (the raw vec is absent). Aggregates two rankings:
/// most_overall (per Class#field, total elements/retained across all its
/// containers, distinct-container count) and biggest_single (per Class#field,
/// the single largest container by element count).
fn build_collection_attribution(
    g: &Graph,
    overview: &SystemOverview,
) -> Option<CollectionAttribution> {
    use std::collections::HashMap;
    let raw = g.collection_attribution_raw.as_ref()?;
    // holder-instance lookup: prettified class name → summed live instances
    // (sum across distinct class-loader rows sharing a pretty name).
    let mut holder_counts: HashMap<String, u64> = HashMap::new();
    for row in &overview.histogram {
        *holder_counts.entry(row.pretty_class.clone()).or_insert(0) += row.instances;
    }
    Some(aggregate_collection_attribution(
        raw,
        &g.retained,
        g.collection_attribution_truncated,
        &holder_counts,
        g.ref_size as u64,
    ))
}

/// Aggregate the raw fields-by-size groups into a ranking of `Class#field` by
/// total retained size of their pointees. `None` when `--collections` was off
/// (the raw vec is absent). For each group: sum `g.retained` over distinct
/// pointee indices, pick the dominant runtime pointee class (by summed
/// retained), and match the holder's live-instance count from the histogram.
/// Sorted by total_retained desc, truncated to `ATTRIBUTION_TOP_N`.
fn build_fields_by_size(g: &Graph, overview: &SystemOverview) -> Option<FieldsBySize> {
    use std::collections::HashMap;
    let raw = g.fields_by_size_raw.as_ref()?;
    // dense idx → element count, for the `elements` column (container pointees).
    let elems_by_idx: std::collections::HashMap<u32, u64> = g
        .coll_values_raw
        .as_ref()
        .map(|cv| {
            cv.iter()
                .map(|c| (c.container_idx, c.value_indices.len() as u64))
                .collect()
        })
        .unwrap_or_default();
    let mut holder_counts: HashMap<String, u64> = HashMap::new();
    for row in &overview.histogram {
        *holder_counts.entry(row.pretty_class.clone()).or_insert(0) += row.instances;
    }

    let mut rows: Vec<FieldBySizeRow> = raw
        .iter()
        .map(|grp| {
            let mut total_retained: u64 = 0;
            // Sum retained per pointee, and tally retained per runtime type to
            // pick the dominant one.
            let mut type_retained: HashMap<String, u64> = HashMap::new();
            for &idx in &grp.pointee_indices {
                let r = g.retained.get(idx as usize).copied().unwrap_or(0);
                total_retained += r;
                *type_retained
                    .entry(class_display(g, idx as usize))
                    .or_insert(0) += r;
            }
            // Dominant pointee type: the class with the most summed retained. If
            // more than one type is present and none has a strict majority of
            // retained, report `varies`.
            let pointee_type = dominant_pointee_type(&type_retained, total_retained);
            let holder_instances = holder_counts
                .get(&pretty_class_name(&grp.holder_class))
                .copied()
                .unwrap_or(0);
            let elements: u64 = grp
                .pointee_indices
                .iter()
                .filter_map(|idx| elems_by_idx.get(idx).copied())
                .sum();
            let category = classify_pointee(&pointee_type);
            FieldBySizeRow {
                holder_class: pretty_class_name(&grp.holder_class),
                field: grp.field.clone(),
                pointee_type,
                total_retained,
                pointees: grp.pointee_indices.len() as u64,
                holder_instances,
                elements,
                category,
            }
        })
        .collect();

    rows.sort_by(|a, b| {
        b.total_retained
            .cmp(&a.total_retained)
            .then(b.pointees.cmp(&a.pointees))
            .then_with(|| a.holder_class.cmp(&b.holder_class))
            .then_with(|| a.field.cmp(&b.field))
    });
    let truncated = rows.len() > ATTRIBUTION_TOP_N;
    rows.truncate(ATTRIBUTION_TOP_N);
    Some(FieldsBySize { rows, truncated })
}

/// Pick the dominant runtime pointee type from a `type → summed retained` map:
/// the single type holding a strict majority (> 50%) of the group's retained
/// size, else `varies` when more than one type is present. A single type always
/// wins.
fn dominant_pointee_type(
    type_retained: &std::collections::HashMap<String, u64>,
    total_retained: u64,
) -> String {
    if type_retained.len() == 1 {
        return type_retained.keys().next().cloned().unwrap_or_default();
    }
    let (best, best_r) = type_retained
        .iter()
        .max_by_key(|(_, r)| **r)
        .map(|(t, r)| (t.clone(), *r))
        .unwrap_or_default();
    if total_retained > 0 && best_r * 2 > total_retained {
        best
    } else {
        "varies".to_string()
    }
}

/// Dominant element type by COUNT (>50% majority), else `varies`. Single type
/// always wins. Mirrors `dominant_pointee_type` but keyed on counts.
fn dominant_value_type(counts: &std::collections::HashMap<String, u64>) -> String {
    if counts.len() == 1 {
        return counts.keys().next().cloned().unwrap_or_default();
    }
    let total: u64 = counts.values().sum();
    let (best, best_c) = counts
        .iter()
        .max_by_key(|(_, c)| **c)
        .map(|(t, c)| (t.clone(), *c))
        .unwrap_or_default();
    if total > 0 && best_c * 2 > total {
        best
    } else {
        "varies".to_string()
    }
}

/// Top-K element types by count (desc), ties broken by type name asc.
fn top_value_shares(
    counts: &std::collections::HashMap<String, u64>,
    k: usize,
) -> Vec<ValueTypeShare> {
    let mut v: Vec<ValueTypeShare> = counts
        .iter()
        .map(|(t, c)| ValueTypeShare {
            type_name: t.clone(),
            count: *c,
        })
        .collect();
    v.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.type_name.cmp(&b.type_name))
    });
    v.truncate(k);
    v
}

/// Bucket a runtime pointee type into "array" / "collection" / "object" for the
/// Fields-by-Size category column. `varies` and empty fall back to "object".
fn classify_pointee(pointee_type: &str) -> String {
    if pointee_type.ends_with("[]") {
        "array".to_string()
    } else if pointee_type.starts_with("java.util.")
        || pointee_type.contains("Map")
        || pointee_type.contains("List")
        || pointee_type.contains("Set")
        || pointee_type.contains("Collection")
        || pointee_type.contains("scala.collection")
    {
        "collection".to_string()
    } else {
        "object".to_string()
    }
}

/// Build the per-instance "biggest collections" ranking from the raw
/// per-collection value tallies. `None` when `--collections` was off. Retained
/// is joined by the collection's dense index; owner is the primary incoming
/// `Class#field` resolved during field-decode. Combined list ranked by retained
/// desc then elements desc then class asc, top `ATTRIBUTION_TOP_N`; per-kind
/// lists reuse that order. Element/value type columns come from the
/// per-collection element-index tally.
fn build_biggest_collections(g: &Graph) -> Option<BiggestCollections> {
    let raw = g.coll_values_raw.as_ref()?;
    const TOP_N: usize = ATTRIBUTION_TOP_N;
    const TOP_K_TYPES: usize = 4;

    let mut rows: Vec<BiggestCollectionRow> = raw
        .iter()
        .map(|c| {
            let mut counts: std::collections::HashMap<String, u64> =
                std::collections::HashMap::new();
            for &vi in &c.value_indices {
                *counts.entry(element_type_display(g, vi)).or_insert(0) += 1;
            }
            let retained = g
                .retained
                .get(c.container_idx as usize)
                .copied()
                .unwrap_or(0);
            BiggestCollectionRow {
                kind: kind_label(c.kind).to_string(),
                container_class: c.container_class.clone(),
                elements: c.value_indices.len() as u64,
                retained: Some(retained),
                owner: c.owner.clone(),
                dominant_value_type: Some(dominant_value_type(&counts)),
                value_type_breakdown: top_value_shares(&counts, TOP_K_TYPES),
                obj_index_1based: c.container_idx.checked_add(1),
            }
        })
        .collect();

    rows.sort_by(|a, b| {
        b.retained
            .unwrap_or(0)
            .cmp(&a.retained.unwrap_or(0))
            .then(b.elements.cmp(&a.elements))
            .then_with(|| a.container_class.cmp(&b.container_class))
    });
    let truncated = rows.len() > TOP_N;
    let combined: Vec<BiggestCollectionRow> = rows.iter().take(TOP_N).cloned().collect();

    const KINDS: [&str; 6] = ["list", "map", "set", "deque", "queue", "tree"];
    let mut by_kind: Vec<CollectionKindTable> = Vec::new();
    for kind in KINDS {
        let mut krows: Vec<BiggestCollectionRow> =
            rows.iter().filter(|r| r.kind == kind).cloned().collect();
        if krows.is_empty() {
            continue;
        }
        krows.truncate(TOP_N);
        by_kind.push(CollectionKindTable {
            kind: kind.to_string(),
            rows: krows,
        });
    }

    Some(BiggestCollections {
        combined,
        by_kind,
        truncated,
    })
}

/// Aggregate the raw per-collection value tallies into a global per-collection-
/// class breakdown: for each container class, sum instances + total values and
/// merge element-type counts, keeping the top types. `None` when `--collections`
/// was off. Sorted by total_values desc, top `ATTRIBUTION_TOP_N`.
fn build_collection_contents(g: &Graph) -> Option<CollectionContents> {
    use std::collections::HashMap;
    let raw = g.coll_values_raw.as_ref()?;
    const TOP_N: usize = ATTRIBUTION_TOP_N;
    const TOP_K_TYPES: usize = 5;

    struct Acc {
        instances: u64,
        total_values: u64,
        type_counts: HashMap<String, u64>,
    }
    let mut by_class: HashMap<String, Acc> = HashMap::new();
    for c in raw {
        let acc = by_class.entry(c.container_class.clone()).or_insert(Acc {
            instances: 0,
            total_values: 0,
            type_counts: HashMap::new(),
        });
        acc.instances += 1;
        acc.total_values += c.value_indices.len() as u64;
        for &vi in &c.value_indices {
            *acc.type_counts
                .entry(element_type_display(g, vi))
                .or_insert(0) += 1;
        }
    }

    let mut rows: Vec<CollectionContentsRow> = by_class
        .into_iter()
        .map(|(collection_class, acc)| CollectionContentsRow {
            collection_class,
            instances: acc.instances,
            total_values: acc.total_values,
            top_value_types: top_value_shares(&acc.type_counts, TOP_K_TYPES),
        })
        .collect();
    rows.sort_by(|a, b| {
        b.total_values
            .cmp(&a.total_values)
            .then_with(|| a.collection_class.cmp(&b.collection_class))
    });
    let truncated = rows.len() > TOP_N;
    rows.truncate(TOP_N);
    Some(CollectionContents { rows, truncated })
}

/// Pure aggregation core (no `Graph` dependency, so it is directly unit
/// testable): fold the raw attribution records into the two `Class#field`
/// rankings, looking up each container's retained size in `retained` by its
/// dense object index. See [`build_collection_attribution`] for the semantics.
fn aggregate_collection_attribution(
    raw: &[AttributionRaw],
    retained: &[u64],
    truncated: bool,
    holder_counts: &std::collections::HashMap<String, u64>,
    obj_ref_width: u64,
) -> CollectionAttribution {
    use std::collections::HashMap;

    // most_overall accumulator, keyed by (holder_class, field).
    struct OverallAcc {
        total_elements: u64,
        total_retained: u64,
        total_wasted_slots: u64,
        // Distinct container indices under this key: powers container_count and
        // dedups elements/retained so a shared container isn't double-counted.
        seen: std::collections::HashSet<u32>,
        // Kind of the FIRST distinct container; `mixed` once a later distinct
        // container disagrees.
        first_kind: u8,
        mixed: bool,
    }
    // biggest_single accumulator, keyed by (holder_class, field).
    struct BiggestAcc {
        elements: u64,
        retained: u64,
        container_class: String,
        capacity: u64,
        container_kind: u8,
    }

    let mut overall: HashMap<(String, String), OverallAcc> = HashMap::new();
    let mut biggest: HashMap<(String, String), BiggestAcc> = HashMap::new();
    // tiny: keyed by (holder_class, field, container_kind), dedup by container_idx.
    struct TinyAcc {
        empty_count: u64,
        singleton_count: u64,
        seen: std::collections::HashSet<u32>,
    }
    let mut tiny: HashMap<(String, String, u8), TinyAcc> = HashMap::new();

    for rec in raw {
        let retained_bytes = retained
            .get(rec.container_idx as usize)
            .copied()
            .unwrap_or(0);
        let key = (rec.holder_class.clone(), rec.field.clone());

        // most_overall: dedup by distinct container index.
        let acc = overall.entry(key.clone()).or_insert_with(|| OverallAcc {
            total_elements: 0,
            total_retained: 0,
            total_wasted_slots: 0,
            seen: std::collections::HashSet::new(),
            first_kind: rec.container_kind,
            mixed: false,
        });
        if acc.seen.insert(rec.container_idx) {
            acc.total_elements += rec.elements;
            acc.total_retained += retained_bytes;
            acc.total_wasted_slots += rec.capacity.saturating_sub(rec.elements);
            // Mixed determination only considers DISTINCT containers.
            if rec.container_kind != acc.first_kind {
                acc.mixed = true;
            }
        }

        // biggest_single: track the single largest container by element count
        // (tie-break larger retained). Idempotent under duplicate container
        // rows, so no dedup is needed.
        let b = biggest.entry(key).or_insert_with(|| BiggestAcc {
            elements: 0,
            retained: 0,
            container_class: String::new(),
            capacity: 0,
            container_kind: rec.container_kind,
        });
        if rec.elements > b.elements || (rec.elements == b.elements && retained_bytes > b.retained)
        {
            b.elements = rec.elements;
            b.retained = retained_bytes;
            b.container_class = crate::report::pretty_class_name(&rec.container_class);
            b.capacity = rec.capacity;
            b.container_kind = rec.container_kind;
        }

        // tiny: count empty/singleton containers per (holder, field, kind).
        if rec.elements <= 1 {
            let ta = tiny
                .entry((
                    rec.holder_class.clone(),
                    rec.field.clone(),
                    rec.container_kind,
                ))
                .or_insert_with(|| TinyAcc {
                    empty_count: 0,
                    singleton_count: 0,
                    seen: std::collections::HashSet::new(),
                });
            if ta.seen.insert(rec.container_idx) {
                if rec.elements == 0 {
                    ta.empty_count += 1;
                } else {
                    ta.singleton_count += 1;
                }
            }
        }
    }

    let mut most_overall: Vec<FieldAttributionRow> = overall
        .into_iter()
        .map(|((holder_class, field), acc)| FieldAttributionRow {
            container_kind: if acc.mixed {
                "mixed".to_string()
            } else {
                kind_label(acc.first_kind).to_string()
            },
            total_elements: acc.total_elements,
            total_retained: acc.total_retained,
            total_wasted_slots: acc.total_wasted_slots,
            total_wasted_bytes: acc.total_wasted_slots.saturating_mul(obj_ref_width),
            container_count: acc.seen.len() as u64,
            holder_instances: holder_counts
                .get(&crate::report::pretty_class_name(&holder_class))
                .copied()
                .unwrap_or(0),
            holder_class,
            field,
        })
        .collect();
    // total_elements desc, total_retained desc, holder_class asc, field asc.
    most_overall.sort_by(|a, b| {
        b.total_elements
            .cmp(&a.total_elements)
            .then(b.total_retained.cmp(&a.total_retained))
            .then_with(|| a.holder_class.cmp(&b.holder_class))
            .then_with(|| a.field.cmp(&b.field))
    });
    most_overall.truncate(ATTRIBUTION_TOP_N);

    let mut biggest_single: Vec<FieldAttributionBiggestRow> = biggest
        .into_iter()
        .map(|((holder_class, field), b)| FieldAttributionBiggestRow {
            holder_class,
            field,
            container_class: b.container_class,
            elements: b.elements,
            retained: b.retained,
            capacity: b.capacity,
            container_kind: kind_label(b.container_kind).to_string(),
        })
        .collect();
    // elements desc, retained desc, holder_class asc, field asc.
    biggest_single.sort_by(|a, b| {
        b.elements
            .cmp(&a.elements)
            .then(b.retained.cmp(&a.retained))
            .then_with(|| a.holder_class.cmp(&b.holder_class))
            .then_with(|| a.field.cmp(&b.field))
    });
    biggest_single.truncate(ATTRIBUTION_TOP_N);

    let mut tiny_overhead: Vec<crate::report::model::TinyCollectionRow> = tiny
        .into_iter()
        .filter_map(|((holder_class, field, kind), ta)| {
            let total = ta.empty_count + ta.singleton_count;
            if total == 0 {
                return None;
            }
            Some(crate::report::model::TinyCollectionRow {
                holder_class,
                field,
                container_kind: kind_label(kind).to_string(),
                empty_count: ta.empty_count,
                singleton_count: ta.singleton_count,
                overhead_bytes: total.saturating_mul(80),
            })
        })
        .collect();
    tiny_overhead.sort_by(|a, b| {
        b.overhead_bytes
            .cmp(&a.overhead_bytes)
            .then_with(|| a.holder_class.cmp(&b.holder_class))
            .then_with(|| a.field.cmp(&b.field))
    });
    tiny_overhead.truncate(20);

    CollectionAttribution {
        most_overall,
        biggest_single,
        tiny_overhead,
        truncated,
    }
}

/// Max components (class loaders) surfaced in the Top Components view.
const TOP_COMPONENTS: usize = 10;
/// Max top classes listed inside each component.
const COMPONENT_TOP_CLASSES: usize = 5;

/// Eclipse-MAT-style "Top Components": group the class histogram by class loader
/// (component) and sum retained heap; report the top components with their top
/// classes. A bounded fold over `overview.histogram` (rows <= #loaders), so
/// RSS-safe. `pct` is against the total reachable retained heap (sum of the
/// histogram's MAT-top-ancestor retained), matching how the histogram reports it.
fn build_top_components(overview: &SystemOverview) -> TopComponents {
    use std::collections::HashMap;

    let total_retained: u64 = overview.histogram.iter().map(|r| r.retained).sum();

    struct Acc {
        label: String,
        retained: u64,
        classes: Vec<ComponentClass>,
    }
    let mut by_loader: HashMap<u64, Acc> = HashMap::new();
    for row in &overview.histogram {
        let label = row
            .loader_label
            .clone()
            .unwrap_or_else(|| format!("loader @ {:#x}", row.loader_id));
        let acc = by_loader.entry(row.loader_id).or_insert_with(|| Acc {
            label,
            retained: 0,
            classes: Vec::new(),
        });
        acc.retained += row.retained;
        acc.classes.push(ComponentClass {
            pretty_class: row.pretty_class.clone(),
            retained: row.retained,
        });
    }

    let mut components: Vec<Component> = by_loader
        .into_values()
        .map(|mut acc| {
            // Top classes within the component, retained desc (tie-break name asc).
            acc.classes.sort_by(|a, b| {
                b.retained
                    .cmp(&a.retained)
                    .then(a.pretty_class.cmp(&b.pretty_class))
            });
            acc.classes.truncate(COMPONENT_TOP_CLASSES);
            let pct = if total_retained > 0 {
                acc.retained as f64 / total_retained as f64 * 100.0
            } else {
                0.0
            };
            Component {
                loader_label: acc.label,
                retained: acc.retained,
                pct,
                top_classes: acc.classes,
            }
        })
        .collect();
    // Components retained desc (tie-break label asc, then the component's top
    // class name asc for a total order — distinct loaders can share a label
    // and retained size, so the label alone is not a stable key).
    components.sort_by(|a, b| {
        b.retained
            .cmp(&a.retained)
            .then(a.loader_label.cmp(&b.loader_label))
            .then_with(|| {
                let ak = a.top_classes.first().map(|c| c.pretty_class.as_str());
                let bk = b.top_classes.first().map(|c| c.pretty_class.as_str());
                ak.cmp(&bk)
            })
    });
    components.truncate(TOP_COMPONENTS);
    TopComponents { components }
}

/// Compute the always-on "Dominator Analysis" (Big Drops + Immediate
/// Dominators) from the already-built dominator structures. RSS-neutral: reads
/// `g.idom`/`g.retained`/`g.shallow`/`g.class_idx` plus the dominator-children
/// CSR passed into `build_model`; the only per-object allocation is a handful of
/// class-indexed tallies (bounded by #classes, like the histogram) plus the
/// capped output row Vecs.
fn build_dominator_analysis(
    g: &Graph,
    dc_offsets: &[u32],
    dc_targets: &[u32],
    longest_chain_depth: u32,
) -> DominatorAnalysis {
    let n = g.n;
    let undef = u32::MAX;
    let class_count = g.class_names.len();
    let dom_children = |node: usize| -> &[u32] {
        &dc_targets[dc_offsets[node] as usize..dc_offsets[node + 1] as usize]
    };
    let display_of = |i: usize| -> String {
        let ci = g.class_idx[i] as usize;
        if ci < class_count {
            pretty_class_name(&g.class_names[ci])
        } else {
            String::new()
        }
    };

    // Total reachable shallow, for the big-drops significance threshold (1%).
    let total_shallow: u64 = (0..n)
        .filter(|&i| g.idom[i] != undef)
        .map(|i| g.shallow[i] as u64)
        .sum();
    const DROP_THRESHOLD_PCT: f64 = 1.0;
    let threshold = (total_shallow as f64 * DROP_THRESHOLD_PCT / 100.0) as u64;

    // ---- Big Drops (#1) ----
    // Walk every reachable node that is itself "significant" (retained >=
    // threshold). For each, find its largest dominator child; a big drop is
    // where retained(node) - retained(largest_child) is large (heap
    // concentrates here rather than flowing to one dominated child).
    let mut drops: Vec<BigDropRow> = Vec::new();
    for i in 0..n {
        if g.idom[i] == undef {
            continue;
        }
        if g.retained[i] < threshold {
            continue;
        }
        let kids = dom_children(i);
        let child_count = kids.len() as u64;
        let (largest_child_retained, largest_child_idx) = kids
            .iter()
            .map(|&c| (g.retained[c as usize], c))
            .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))
            .unwrap_or((0, u32::MAX));
        let drop_bytes = g.retained[i].saturating_sub(largest_child_retained);
        if drop_bytes == 0 {
            continue;
        }
        drops.push(BigDropRow {
            obj_index_1based: (i as u64) + 1,
            display_class: display_of(i),
            retained: g.retained[i],
            child_count,
            largest_child_retained,
            largest_child_class: if largest_child_idx != u32::MAX {
                display_of(largest_child_idx as usize)
            } else {
                String::new()
            },
            drop_bytes,
        });
    }
    drops.sort_unstable_by(|a, b| {
        b.drop_bytes
            .cmp(&a.drop_bytes)
            .then(a.obj_index_1based.cmp(&b.obj_index_1based))
    });
    drops.truncate(BIG_DROPS_CAP);
    let big_drops = BigDrops {
        threshold,
        rows: drops,
    };

    // ---- Immediate Dominators (#2) ----
    // For dominator node p (any reachable node with >=1 dom child), key the
    // rollup by class_of(p). Sum dominated_count/dominated_shallow over p's
    // children; count p once in dominator_count and add its shallow. Class
    // keys are folded through `class_row_remap` so they match the main
    // histogram.
    //
    // Simultaneously build a (dominator_class, dominated_class) pair map for
    // the V5 two-sided sankey. The pair map uses (remapped_parent_ci,
    // remapped_child_ci) as the key and accumulates (pair_count,
    // dominated_shallow, dominated_retained).
    let remap = class_row_remap(g);
    let mut dom_count = vec![0u64; class_count]; // #dominator objects of this class
    let mut domd_count = vec![0u64; class_count]; // #objects immediately dominated
    let mut dom_shallow = vec![0u64; class_count];
    let mut domd_shallow = vec![0u64; class_count];
    // pair_map: (parent_remapped_ci, child_remapped_ci) -> (count, shallow, retained)
    let mut pair_map: std::collections::HashMap<(u32, u32), (u64, u64, u64)> =
        std::collections::HashMap::new();
    for p in 0..n {
        if g.idom[p] == undef {
            continue;
        }
        let kids = dom_children(p);
        if kids.is_empty() {
            continue;
        }
        let pci = g.class_idx[p] as usize;
        if pci >= class_count {
            continue;
        }
        let pci = remap[pci] as usize;
        dom_count[pci] += 1;
        dom_shallow[pci] += g.shallow[p] as u64;
        for &c in kids {
            let cu = c as usize;
            domd_count[pci] += 1;
            domd_shallow[pci] += g.shallow[cu] as u64;
            // pair aggregation
            let cci_raw = g.class_idx[cu] as usize;
            if cci_raw < class_count {
                let cci = remap[cci_raw];
                let e = pair_map.entry((pci as u32, cci)).or_insert((0, 0, 0));
                e.0 += 1;
                e.1 += g.shallow[cu] as u64;
                e.2 += g.retained[cu];
            }
        }
    }
    let mut order: Vec<usize> = (0..class_count)
        .filter(|&ci| remap[ci] as usize == ci && dom_count[ci] > 0)
        .collect();
    order.sort_unstable_by(|&a, &b| {
        domd_shallow[b]
            .cmp(&domd_shallow[a])
            .then(domd_count[b].cmp(&domd_count[a]))
            .then(a.cmp(&b))
    });
    order.truncate(IMMEDIATE_DOMINATORS_CAP);
    let rows: Vec<ImmediateDominatorRow> = order
        .into_iter()
        .map(|ci| ImmediateDominatorRow {
            dominator_class: pretty_class_name(&g.class_names[ci]),
            dominator_count: dom_count[ci],
            dominated_count: domd_count[ci],
            dominator_shallow: dom_shallow[ci],
            dominated_shallow: domd_shallow[ci],
        })
        .collect();
    // Sort pairs by dominated_retained desc, cap.
    let mut pairs_vec: Vec<ImmDomPair> = pair_map
        .into_iter()
        .map(|((pci, cci), (cnt, shl, ret))| ImmDomPair {
            dominator_class: pretty_class_name(&g.class_names[pci as usize]),
            dominated_class: pretty_class_name(&g.class_names[cci as usize]),
            pair_count: cnt,
            dominated_shallow: shl,
            dominated_retained: ret,
        })
        .collect();
    pairs_vec.sort_unstable_by(|a, b| {
        b.dominated_retained
            .cmp(&a.dominated_retained)
            .then(a.dominator_class.cmp(&b.dominator_class))
            .then(a.dominated_class.cmp(&b.dominated_class))
    });
    pairs_vec.truncate(IMDOM_PAIRS_CAP);
    let immediate_dominators = ImmediateDominators {
        rows,
        pairs: pairs_vec,
    };

    DominatorAnalysis {
        big_drops,
        immediate_dominators,
        longest_chain_depth,
    }
}

/// Decode a raw `java.lang.Thread.threadStatus` value into a MAT-style state
/// label like `[alive, runnable]`. The low bits are the JVMTI thread-state bit
/// field (`JVMTI_THREAD_STATE_*`). Mirrors Eclipse MAT's `getThreadState`.
fn thread_state_label(status: i32) -> String {
    // JVMTI thread-state bit constants.
    const ALIVE: i32 = 0x0001;
    const TERMINATED: i32 = 0x0002;
    const RUNNABLE: i32 = 0x0004;
    const BLOCKED_ON_MONITOR: i32 = 0x0400;
    const WAITING: i32 = 0x0080;
    const WAITING_INDEFINITELY: i32 = 0x0010;
    const WAITING_WITH_TIMEOUT: i32 = 0x0020;
    const SLEEPING: i32 = 0x0040;
    const IN_OBJECT_WAIT: i32 = 0x0100;
    const PARKED: i32 = 0x0200;

    let mut parts: Vec<&str> = Vec::new();
    if status & ALIVE != 0 {
        parts.push("alive");
    }
    if status & TERMINATED != 0 {
        parts.push("terminated");
    }
    if status & RUNNABLE != 0 {
        parts.push("runnable");
    }
    if status & BLOCKED_ON_MONITOR != 0 {
        parts.push("blocked on monitor");
    }
    if status & WAITING != 0 {
        parts.push("waiting");
    }
    if status & WAITING_INDEFINITELY != 0 {
        parts.push("waiting indefinitely");
    }
    if status & WAITING_WITH_TIMEOUT != 0 {
        parts.push("waiting with timeout");
    }
    if status & SLEEPING != 0 {
        parts.push("sleeping");
    }
    if status & IN_OBJECT_WAIT != 0 {
        parts.push("in Object.wait");
    }
    if status & PARKED != 0 {
        parts.push("parked");
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("[{}]", parts.join(", "))
    }
}

/// Aggregate per-object allocation stack-trace serials into per-site totals.
/// Skips serial 0 (no allocation info). Returns `traces_present: false` with an
/// empty site list when every object has serial 0 (allocation tracking off).
/// Otherwise sorts by object count desc (tie-break retained_total desc, then
/// stack_serial asc), keeps the top `top_n`, and resolves frame lines from
/// `g.alloc_frames_by_serial`.
pub(crate) fn build_alloc_sites(g: &Graph, top_n: usize) -> AllocSites {
    build_alloc_sites_from(g, top_n, g.alloc_stack_serial.iter().copied())
}

/// Streaming accumulator for alloc-site aggregation. Objects are fed one serial
/// at a time in index order via [`AllocAgg::push`]; [`AllocAgg::finish`] produces
/// the top-N `AllocSites`. This lets the big-dump path feed serials as they are
/// stream-decompressed from a deflate blob (via `CompressedU32::for_each_u32`),
/// never materialising a second ~2GB buffer alongside the decompressed bytes.
pub(crate) struct AllocAgg<'g> {
    g: &'g Graph,
    top_n: usize,
    idx: usize,
    // (object_count, shallow_total, retained_total) keyed by stack serial.
    agg: std::collections::HashMap<u32, (u64, u64, u64)>,
}

impl<'g> AllocAgg<'g> {
    pub(crate) fn new(g: &'g Graph, top_n: usize) -> Self {
        Self {
            g,
            top_n,
            idx: 0,
            agg: std::collections::HashMap::new(),
        }
    }

    /// Feed the serial for the next object index (in index order).
    pub(crate) fn push(&mut self, serial: u32) {
        let i = self.idx;
        self.idx += 1;
        if serial == 0 {
            return;
        }
        // Guard against a blob/graph length mismatch: `i` tracks the number of
        // serials streamed in, which must equal the object count. If a corrupt
        // or mismatched alloc-serial blob decodes to more entries than the
        // graph has objects, index directly would panic; skip the overrun
        // instead so aggregation degrades gracefully rather than crashing.
        if i >= self.g.shallow.len() {
            return;
        }
        let e = self.agg.entry(serial).or_insert((0, 0, 0));
        e.0 += 1;
        e.1 += self.g.shallow[i] as u64;
        e.2 += self.g.retained[i];
    }

    pub(crate) fn finish(self) -> AllocSites {
        if self.agg.is_empty() {
            return AllocSites {
                traces_present: false,
                sites: vec![],
            };
        }
        let empty_frames: Vec<String> = Vec::new();
        let mut sites: Vec<AllocSite> = self
            .agg
            .into_iter()
            .map(
                |(stack_serial, (object_count, shallow_total, retained_total))| {
                    let frames = self
                        .g
                        .alloc_frames_by_serial
                        .as_ref()
                        .and_then(|m| m.get(&stack_serial))
                        .cloned()
                        .unwrap_or_else(|| empty_frames.clone());
                    AllocSite {
                        stack_serial,
                        frames,
                        object_count,
                        shallow_total,
                        retained_total,
                    }
                },
            )
            .collect();
        // Deterministic ordering: object_count desc, then retained_total desc,
        // then stack_serial asc.
        sites.sort_by(|a, b| {
            b.object_count
                .cmp(&a.object_count)
                .then_with(|| b.retained_total.cmp(&a.retained_total))
                .then_with(|| a.stack_serial.cmp(&b.stack_serial))
        });
        sites.truncate(self.top_n);
        AllocSites {
            traces_present: true,
            sites,
        }
    }
}

/// Core alloc-site aggregation, parameterised by the per-object serial source
/// (`serials` yields one serial per object index, in index order). This lets the
/// caller feed serials either from the dense `g.alloc_stack_serial` Vec or by
/// streaming them out of a decompressed byte buffer — avoiding materialising a
/// second ~2GB `Vec<u32>` alongside the decompressed bytes on the big dump.
pub(crate) fn build_alloc_sites_from<I: Iterator<Item = u32>>(
    g: &Graph,
    top_n: usize,
    serials: I,
) -> AllocSites {
    let mut agg = AllocAgg::new(g, top_n);
    for serial in serials {
        agg.push(serial);
    }
    agg.finish()
}

/// Resolve each thread stack into a `ThreadInfo`. The thread's class name is
/// looked up via its object index (`u32::MAX` = unresolved). Small: one entry
/// per stack trace.
pub(crate) fn build_thread_overview(g: &Graph, total_shallow: u64) -> ThreadOverview {
    let threads = g
        .thread_stacks
        .iter()
        .map(|t| {
            let class_name = if t.thread_obj_idx == u32::MAX {
                None
            } else {
                g.class_idx
                    .get(t.thread_obj_idx as usize)
                    .and_then(|&ci| g.class_names.get(ci as usize))
                    .cloned()
            };
            let local_objects = Some(
                g.thread_local_samples
                    .get(&t.thread_serial)
                    .map(|idxs| {
                        let mut objs: Vec<ThreadLocalObj> = idxs
                            .iter()
                            .map(|&li| {
                                let display_class = g
                                    .class_idx
                                    .get(li as usize)
                                    .and_then(|&ci| g.class_names.get(ci as usize))
                                    .map(|s| pretty_class_name(s))
                                    .unwrap_or_else(|| "<unknown>".to_string());
                                ThreadLocalObj {
                                    obj_index_1based: li as usize + 1,
                                    display_class,
                                    shallow: g.shallow[li as usize] as u64,
                                    retained: g.retained[li as usize],
                                }
                            })
                            .collect();
                        // Retained desc; tie-break on 1-based index asc for determinism.
                        objs.sort_by(|a, b| {
                            b.retained
                                .cmp(&a.retained)
                                .then(a.obj_index_1based.cmp(&b.obj_index_1based))
                        });
                        objs
                    })
                    .unwrap_or_default(),
            );

            // Thread object footprint (shallow/retained) from its heap index.
            let (shallow, retained) = if t.thread_obj_idx == u32::MAX {
                (0, 0)
            } else {
                let idx = t.thread_obj_idx as usize;
                (
                    g.shallow.get(idx).copied().unwrap_or(0) as u64,
                    g.retained.get(idx).copied().unwrap_or(0),
                )
            };

            // Always-on Thread properties (daemon/priority/state/context loader).
            let props = g.thread_props.get(&t.thread_serial);
            let is_daemon = props.map(|p| p.is_daemon).unwrap_or(false);
            let priority = props.map(|p| p.priority).unwrap_or(0);
            let thread_state = props
                .map(|p| thread_state_label(p.thread_status))
                .unwrap_or_default();
            let context_class_loader = props
                .map(|p| p.context_loader_addr)
                .filter(|&a| a != 0)
                .map(|addr| loader_label_for_addr(g, addr));

            // Gated per-frame significant locals (only when --thread-locals ran).
            let (significant_frames, max_local_retained) =
                build_significant_frames(g, t, total_shallow);

            ThreadInfo {
                thread_serial: t.thread_serial,
                name: g
                    .thread_props
                    .get(&t.thread_serial)
                    .map(|p| p.name.clone())
                    .filter(|s| !s.is_empty()),
                class_name,
                frames: t.frames.clone(),
                local_root_count: g
                    .thread_local_counts
                    .get(&t.thread_serial)
                    .copied()
                    .unwrap_or(0),
                local_objects,
                shallow,
                retained,
                max_local_retained,
                context_class_loader,
                is_daemon,
                priority,
                thread_state,
                significant_frames,
            }
        })
        .collect();
    ThreadOverview { threads }
}

/// Resolve a context-class-loader object address to a display label
/// `ClassName @ 0xADDR`. Falls back to a bare address when the class of the
/// loader object cannot be resolved.
fn loader_label_for_addr(g: &Graph, addr: u64) -> String {
    // loader_labels is keyed by loader object address → its class NAME label.
    if let Some(label) = g.loader_labels.get(&addr) {
        return format!("{label} @ {addr:#x}");
    }
    format!("@ {addr:#x}")
}

/// Build the per-frame significant-locals interleave for one thread from the
/// gated `thread_local_frame_samples`. Returns the frames (top-first) with their
/// significant local objects (retained desc) plus the max local retained. Empty
/// when `--thread-locals` was not set (the gated map is empty).
fn build_significant_frames(
    g: &Graph,
    t: &crate::pass2::ThreadStack,
    total_shallow: u64,
) -> (Vec<SignificantFrame>, u64) {
    use std::collections::BTreeMap;
    let Some(pairs) = g.thread_local_frame_samples.get(&t.thread_serial) else {
        return (Vec::new(), 0);
    };
    if pairs.is_empty() {
        return (Vec::new(), 0);
    }
    // Group local indices by frame_number (u32::MAX = no-frame bucket, rendered last).
    let mut by_frame: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &(frame_number, local_idx) in pairs {
        by_frame.entry(frame_number).or_default().push(local_idx);
    }

    let mut max_local_retained: u64 = 0;
    let mut frames_out: Vec<SignificantFrame> = Vec::new();
    for (&frame_number, locals) in &by_frame {
        // Frame label: the rendered frame line, or a synthetic label for the
        // no-frame bucket (JNI locals / native stack / thread block).
        let frame = if frame_number == u32::MAX {
            "<no frame> (JNI local / native stack)".to_string()
        } else {
            t.frames
                .get(frame_number as usize)
                .cloned()
                .unwrap_or_else(|| format!("<frame #{frame_number}>"))
        };
        let mut locals_out: Vec<SignificantLocal> = locals
            .iter()
            .map(|&li| {
                let display_class = g
                    .class_idx
                    .get(li as usize)
                    .and_then(|&ci| g.class_names.get(ci as usize))
                    .map(|s| pretty_class_name(s))
                    .unwrap_or_else(|| "<unknown>".to_string());
                let retained = g.retained.get(li as usize).copied().unwrap_or(0);
                max_local_retained = max_local_retained.max(retained);
                let pct = if total_shallow > 0 {
                    retained as f64 / total_shallow as f64 * 100.0
                } else {
                    0.0
                };
                SignificantLocal {
                    display_class: pretty_class_name(&display_class),
                    retained,
                    pct,
                }
            })
            .collect();
        // Retained desc, tie-break class name asc for determinism.
        locals_out.sort_by(|a, b| {
            b.retained
                .cmp(&a.retained)
                .then(a.display_class.cmp(&b.display_class))
        });
        frames_out.push(SignificantFrame {
            frame,
            locals: locals_out,
        });
    }
    (frames_out, max_local_retained)
}

/// Compute the heap fragmentation ratio: unreachable shallow heap as a fraction
/// of total heap (reachable + unreachable). Returns 0.0 for an empty heap.
fn compute_fragmentation_ratio(total_shallow: u64, unreachable_shallow: u64) -> f64 {
    let denom = total_shallow + unreachable_shallow;
    if denom == 0 {
        0.0
    } else {
        unreachable_shallow as f64 / denom as f64
    }
}

/// Compute the retained heap share of the single largest class in integer basis
/// points (100 bp = 1%). The histogram must already be sorted by retained
/// descending (as produced by `build_system_overview`). Returns 0 when empty.
fn compute_top_class_concentration_bp(
    histogram: &[crate::report::HistRow],
    total_retained: u64,
) -> u32 {
    if total_retained == 0 {
        return 0;
    }
    histogram
        .first()
        .map(|r| ((r.retained.saturating_mul(10_000)) / total_retained).min(10_000) as u32)
        .unwrap_or(0)
}

/// Aggregate all "System Overview" scalars, the class histogram, and the
/// derived breakdowns (GC-roots-by-type, heap composition, dominator-depth
/// histogram, retention concentration, loader rollup, duplicate classes) in a
/// bounded set of passes over the graph. Injects MAT's synthetic
/// `<system class loader>` object where MAT counts it, so totals match bit-exactly.
fn build_system_overview(
    g: &Graph,
    depth_counts: &[u64],
    top_n: usize,
    hist_root_path_top: usize,
) -> (SystemOverview, Vec<u32>) {
    let n = g.n;

    // Count reachable objects and total shallow; track unreachable in the same loop.
    // Hoisted here (also used by the reachable class histogram below) so the
    // duplicate-row remap is computed once.
    let class_count = g.class_names.len();
    // Fold duplicate `java/lang/Class` rows (primitive-type Class mirrors are
    // parsed as plain instances in a separate row) into the single canonical
    // row so histograms count by object type, matching MAT.
    let remap = class_row_remap(g);
    let mut total_objects: u64 = 0;
    let mut total_shallow: u64 = 0;
    let mut unreachable_count: u64 = 0;
    let mut unreachable_shallow: u64 = 0;
    let mut unreach_count: Vec<u64> = vec![0; class_count];
    let mut unreach_shallow: Vec<u64> = vec![0; class_count];
    const KIND_ORDER: [&str; 4] = [
        "Instances",
        "Object Arrays",
        "Primitive Arrays",
        "Class Objects",
    ];
    // Composition bucket index for object `i`, given its already-computed
    // `class_obj_repr` (`repr`). This is `KIND_ORDER.position(object_kind(g,i))`
    // computed WITHOUT the string round-trip and WITHOUT re-probing the
    // class-object HashMap: `object_kind` returns "Class Objects" iff
    // `class_obj_repr != MAX`, so the caller passes the `repr` it already holds.
    // Bucket ids match KIND_ORDER: 0=Instances, 1=Object Arrays,
    // 2=Primitive Arrays, 3=Class Objects.
    let kind_idx_of = |g: &Graph, i: usize, repr: u32| -> usize {
        if repr != u32::MAX {
            return 3; // Class Objects
        }
        match g.class_names.get(g.class_idx[i] as usize) {
            Some(raw) if is_prim_array_desc(raw) => 2, // Primitive Arrays
            Some(raw) if raw.starts_with('[') => 1,    // Object Arrays
            _ => 0,                                    // Instances
        }
    };
    let mut comp_objs = [0u64; 4];
    let mut comp_sh = [0u64; 4];
    // Same kind buckets, but for the UNREACHABLE heap (mirrors comp_objs/comp_sh).
    let mut unreach_comp_objs = [0u64; 4];
    let mut unreach_comp_sh = [0u64; 4];
    // Per primitive-element-type breakdown for unreachable prim arrays:
    // keys are human names like "byte[]"; collected then sorted by shallow desc.
    let mut unreach_prim_by_type: std::collections::HashMap<&'static str, (u64, u64)> =
        std::collections::HashMap::new();
    // retention_concentration: top-100 buffer instead of a ~2.6GB Vec<u64>.
    // Stores the top-100 retained values sorted descending (buf[0] = max,
    // buf[len-1] = current minimum). Only the minimum slot is ever evicted.
    // After the loop: total_retained / prefix sums / num_ge_1pct are all
    // derived from this tiny buffer — no 329M-element sort.
    let vroot_u32 = n as u32;
    let undef_u32 = u32::MAX;
    let mut top100_buf = [0u64; 100];
    let mut top100_len: usize = 0;
    let mut top_total_retained: u64 = 0;
    // classes_loaded and classloaders_loaded (class-object walk)
    let mut classes_loaded: u64 = 0;
    let mut loader_set: std::collections::HashSet<u64> = std::collections::HashSet::new();
    // Class histogram
    let mut inst_count: Vec<u64> = vec![0; class_count];
    let mut shallow_total: Vec<u64> = vec![0; class_count];
    let mut class_retained: Vec<u64> = vec![0; class_count];
    let mut max_shallow: Vec<u64> = vec![0; class_count];
    // Per-class incoming reference count, remapped like the other per-class tallies.
    let mut incoming_ref: Vec<u64> = vec![0; class_count];
    for (ci_raw, &cnt) in g.incoming_refs_per_class.iter().enumerate() {
        if ci_raw < class_count {
            incoming_ref[remap[ci_raw] as usize] += cnt;
        }
    }

    let mut top_level_list: Vec<u32> = Vec::new();
    // Single fused pass over all objects — computes totals, composition,
    // top-level retained, class histogram, and class-loader rollup together
    // to avoid 5 separate O(n) scans on large dumps.
    for i in 0..n {
        let id = g.idom[i];
        let sh = g.shallow[i] as u64;
        let ci_raw = g.class_idx[i] as usize;
        // Resolve class_obj_repr ONCE (a HashMap probe) — it drives the kind
        // bucket, the class-object rollup, AND the loader lookup below, which
        // previously re-probed the same map two more times per object.
        let repr = class_obj_repr(g, i);
        if id != undef_u32 {
            total_objects += 1;
            total_shallow += sh;
            let b = kind_idx_of(g, i, repr);
            comp_objs[b] += 1;
            comp_sh[b] += sh;
            // Retention concentration: maintain top-100 buffer (no 329M Vec).
            if id == vroot_u32 {
                let ret = g.retained[i];
                top_level_list.push(i as u32);
                top_total_retained += ret;
                // Insert into sorted-descending top100_buf if it belongs.
                if top100_len < 100 {
                    top100_buf[top100_len] = ret;
                    top100_len += 1;
                    // Insertion-sort the new element into position.
                    let mut j = top100_len - 1;
                    while j > 0 && top100_buf[j] > top100_buf[j - 1] {
                        top100_buf.swap(j, j - 1);
                        j -= 1;
                    }
                } else if ret > top100_buf[99] {
                    // Evict minimum, insert new value, bubble up.
                    top100_buf[99] = ret;
                    let mut j = 99usize;
                    while j > 0 && top100_buf[j] > top100_buf[j - 1] {
                        top100_buf.swap(j, j - 1);
                        j -= 1;
                    }
                }
            }
            // Class histogram
            if ci_raw < class_count {
                let ci = remap[ci_raw] as usize;
                inst_count[ci] += 1;
                shallow_total[ci] += sh;
                if sh > max_shallow[ci] {
                    max_shallow[ci] = sh;
                }
                if !g.has_same_class_ancestor.get(i) {
                    class_retained[ci] += g.retained[i];
                }
            }
            // Class object: add its retained to the represented class row,
            // and track classes_loaded / loader set.
            if repr != undef_u32 {
                if (repr as usize) < class_count {
                    let ci = remap[repr as usize] as usize;
                    class_retained[ci] += g.retained[i];
                }
                classes_loaded += 1;
                // `repr` IS `class_obj_class_idx[i]`, so reuse it as the row
                // index instead of re-probing the map.
                let lid = g.class_loader_id.get(repr as usize).copied().unwrap_or(0);
                loader_set.insert(lid);
            }
        } else {
            unreachable_count += 1;
            unreachable_shallow += sh;
            let b = kind_idx_of(g, i, repr);
            unreach_comp_objs[b] += 1;
            unreach_comp_sh[b] += sh;
            // Track prim-array sub-types for the composition chart.
            if b == 2 {
                if let Some(raw) = g.class_names.get(ci_raw) {
                    let human: &'static str = match raw.as_str() {
                        "[B" => "byte[]",
                        "[I" => "int[]",
                        "[C" => "char[]",
                        "[J" => "long[]",
                        "[D" => "double[]",
                        "[F" => "float[]",
                        "[S" => "short[]",
                        "[Z" => "boolean[]",
                        _ => "prim[]",
                    };
                    let e = unreach_prim_by_type.entry(human).or_insert((0, 0));
                    e.0 += 1;
                    e.1 += sh;
                }
            }
            if ci_raw < class_count {
                let ci = remap[ci_raw] as usize;
                unreach_count[ci] += 1;
                unreach_shallow[ci] += sh;
            }
        }
    }
    let classloaders_loaded = loader_set.len() as u64;

    // MAT materializes a synthetic <system class loader> object at 0x0
    // (class java/lang/ClassLoader, no HPROF record). Inject its count +
    // shallow so total_objects/total_shallow match MAT bit-exactly. The
    // object has no outbound edges, so nothing else (gc_roots, retained,
    // classes_loaded) is affected — see build_system_overview docs.
    if let Some(sz) = g.system_classloader_shallow {
        total_objects += 1;
        total_shallow += sz as u64;
    }

    let gc_roots = (g
        .gc_root_indices
        .len()
        .saturating_sub(g.synthetic_root_count)) as u64;
    // Break the roots down by HPROF type. Synthetic roots the analyzer injects
    // are all ROOT_SYSTEM_CLASS; subtract them from that bucket so the rows sum
    // to the reported `gc_roots` scalar. Sort by count desc, then label asc.
    let gc_roots_by_type = {
        let mut counts: std::collections::HashMap<&'static str, u64> =
            std::collections::HashMap::new();
        for &ty in &g.gc_root_types {
            *counts.entry(gc_root_type_label(ty)).or_insert(0) += 1;
        }
        if g.synthetic_root_count > 0 {
            let sys = gc_root_type_label(crate::types::heap::ROOT_SYSTEM_CLASS);
            if let Some(c) = counts.get_mut(sys) {
                *c = c.saturating_sub(g.synthetic_root_count as u64);
                if *c == 0 {
                    counts.remove(sys);
                }
            }
        }
        let mut rows: Vec<GcRootTypeRow> = counts
            .into_iter()
            .map(|(root_type, count)| GcRootTypeRow {
                root_type: root_type.to_string(),
                count,
            })
            .collect();
        rows.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.root_type.cmp(&b.root_type))
        });
        rows
    };
    // B5: heap composition by kind. Fixed 4-bucket order.
    let heap_composition = {
        let mut objs = comp_objs;
        let mut sh = comp_sh;
        // Synthetic <system class loader> counts as an Instance, matching how
        // total_objects/total_shallow count it above.
        if let Some(sz) = g.system_classloader_shallow {
            let b = 0; // "Instances" bucket in KIND_ORDER
            objs[b] += 1;
            sh[b] += sz as u64;
        }
        let by_kind = KIND_ORDER
            .iter()
            .enumerate()
            .filter(|&(b, _)| objs[b] > 0)
            .map(|(b, &k)| KindStat {
                kind: k.to_string(),
                objects: objs[b],
                shallow_heap: sh[b],
            })
            .collect();
        HeapComposition {
            by_kind,
            prim_array_by_type: vec![],
        }
    };
    // Unreachable-heap composition by kind (mirrors heap_composition; no
    // synthetic class-loader injection — that object is always reachable).
    let unreachable_composition = {
        let by_kind = KIND_ORDER
            .iter()
            .enumerate()
            .filter(|&(b, _)| unreach_comp_objs[b] > 0)
            .map(|(b, &k)| KindStat {
                kind: k.to_string(),
                objects: unreach_comp_objs[b],
                shallow_heap: unreach_comp_sh[b],
            })
            .collect();
        // Per-type breakdown for primitive arrays; sort by shallow desc,
        // include only when 2+ distinct types are present.
        let mut prim_array_by_type: Vec<KindStat> = unreach_prim_by_type
            .into_iter()
            .map(|(name, (objects, shallow_heap))| KindStat {
                kind: name.to_string(),
                objects,
                shallow_heap,
            })
            .collect();
        prim_array_by_type.sort_by_key(|k| std::cmp::Reverse(k.shallow_heap));
        if prim_array_by_type.len() < 2 {
            prim_array_by_type.clear();
        }
        HeapComposition {
            by_kind,
            prim_array_by_type,
        }
    };
    // B2: dominator-depth histogram (depth = # idom hops up to vroot; 1 =
    // directly under vroot). The per-depth counts were tallied for free during
    // compute_retained's dominator-tree DFS (depth_counts[d-1] = objects at
    // depth d), so no separate ~2GB per-object memo scan runs here. Emit only
    // non-empty buckets, ascending by depth — identical to the old BTreeMap
    // output (which likewise skipped absent depths).
    let dominator_depth_histogram: Vec<DepthBucket> = depth_counts
        .iter()
        .enumerate()
        .filter(|&(_, &objects)| objects > 0)
        .map(|(i, &objects)| DepthBucket {
            depth: (i + 1) as u32,
            objects,
        })
        .collect();
    // B3: retention concentration over top-level dominators (idom == vroot).
    let retention_concentration = {
        let denom = total_shallow.max(1);
        let bp = |sum: u64| -> u32 { ((sum as u128 * 10_000) / denom as u128) as u32 };
        let top_k = |k: usize| -> u64 { top100_buf[..top100_len.min(k)].iter().sum() };
        let total_retained = top_total_retained;
        let one_pct = denom / 100;
        // Count objects with retained >= one_pct.  The top-100 buffer covers the
        // common case (one_pct > top100_buf[99] || top100_len < 100); the rare
        // fallback re-scans all top-level dominators only when many objects exceed
        // the 1%-of-total-shallow threshold.
        let num_objects_ge_1pct = if one_pct == 0 {
            // Degenerate: total_shallow is 0, threshold is 0, every top-level
            // dominator with retained >= 0 qualifies — i.e., all of them.
            let mut cnt = 0u64;
            for i in 0..n {
                if g.idom[i] == vroot_u32 {
                    cnt += 1;
                }
            }
            cnt
        } else if top100_len < 100 || top100_buf[99] < one_pct {
            // All qualifying objects are already in the buffer.
            top100_buf[..top100_len]
                .iter()
                .filter(|&&r| r >= one_pct)
                .count() as u64
        } else {
            // Full scan needed: >= 100 objects exceed the threshold (uncommon).
            let mut cnt = 0u64;
            for i in 0..n {
                if g.idom[i] == vroot_u32 && g.retained[i] >= one_pct {
                    cnt += 1;
                }
            }
            cnt
        };
        let top1_retained = top_k(1);
        let top10_retained = top_k(10);
        let top100_retained = top_k(100);
        RetentionSummary {
            total_retained,
            top1_bp: bp(top1_retained),
            top10_bp: bp(top10_retained),
            top100_bp: bp(top100_retained),
            top1_retained,
            top10_retained,
            top100_retained,
            num_objects_ge_1pct,
        }
    };

    // Inject the synthetic <system class loader> object into its class row so
    // the histogram totals also match MAT. Find the canonical row whose pretty
    // name is java.lang.ClassLoader; add +1 instance / +sz shallow (retained
    // unchanged — the object has no retained subtree).
    if let Some(sz) = g.system_classloader_shallow {
        for ci in 0..class_count {
            if remap[ci] as usize == ci
                && pretty_class_name(&g.class_names[ci]) == "java.lang.ClassLoader"
            {
                inst_count[ci] += 1;
                shallow_total[ci] += sz as u64;
                break;
            }
        }
    }

    // Explicit tie-breaker on ascending class index so equal-retained rows are
    // deterministic. No truncation — `histogram_truncated_to` stays None.
    // Skip rows folded into a canonical row (their tallies moved to the
    // canonical `java/lang/Class` row, leaving them empty).
    let mut order: Vec<usize> = (0..class_count)
        .filter(|&ci| remap[ci] as usize == ci)
        .collect();
    order.sort_unstable_by(|&a, &b| class_retained[b].cmp(&class_retained[a]).then(a.cmp(&b)));
    let mut histogram: Vec<HistRow> = order
        .iter()
        .copied()
        .map(|ci| HistRow {
            pretty_class: pretty_class_name(&g.class_names[ci]),
            instances: inst_count[ci],
            shallow: shallow_total[ci],
            retained: class_retained[ci],
            max_instance_shallow: max_shallow[ci],
            incoming_ref_count: incoming_ref[ci],
            loader_id: g.class_loader_id.get(ci).copied().unwrap_or(0),
            loader_label: {
                // `ci` is the histogram row index, aligned with class_loader_id.
                let lid = g.class_loader_id.get(ci).copied().unwrap_or(0);
                if lid == 0 {
                    Some("<boot>".to_string())
                } else {
                    g.loader_labels.get(&lid).cloned()
                }
            },
            root_path: None,
        })
        .collect();

    // Populate root_path for all histogram rows by retained heap.
    // Populate root_path for the top-20 histogram rows by retained heap.
    // Single O(n) scan to find the highest-retained object per class, then
    // walk the dominator chain for the top-20 rows only.
    if !g.idom.is_empty() && !g.retained.is_empty() {
        const HIST_ROOT_PATH_DEPTH: usize = 30;

        let top_count = histogram.len().min(hist_root_path_top);

        // Collect the class indices we care about (top-20 by retained).
        let top_class_indices: std::collections::HashSet<u32> =
            order[..top_count].iter().map(|&ci| ci as u32).collect();

        // Single O(n) scan: for each object, if its class is in top-20, update best.
        // best_per_ci: class_index → (obj_index, retained)
        let undef_idom = u32::MAX;
        let mut best_per_ci: std::collections::HashMap<u32, (usize, u64)> =
            std::collections::HashMap::new();
        for i in 0..n {
            let ci = g.class_idx.get(i).copied().unwrap_or(u32::MAX);
            if !top_class_indices.contains(&ci) {
                continue;
            }
            if g.idom.get(i).copied().unwrap_or(undef_idom) == undef_idom {
                continue;
            }
            let ret = g.retained.get(i).copied().unwrap_or(0);
            let entry = best_per_ci.entry(ci).or_insert((i, ret));
            if ret > entry.1 {
                *entry = (i, ret);
            }
        }

        // Build GC-root type map.
        let mut root_type_of: std::collections::HashMap<u32, u8> = std::collections::HashMap::new();
        for (idx, &ty) in g.gc_root_indices.iter().zip(g.gc_root_types.iter()) {
            root_type_of
                .entry(*idx)
                .and_modify(|e| *e = (*e).min(ty))
                .or_insert(ty);
        }

        let vroot = n as u32;

        for hist_pos in 0..top_count {
            let ci = order[hist_pos] as u32;
            let Some(&(start_obj, _)) = best_per_ci.get(&ci) else {
                continue;
            };

            let mut chain: Vec<RootPathStep> = Vec::new();
            let mut cur = start_obj as u32;
            for _ in 0..HIST_ROOT_PATH_DEPTH {
                let cur_usize = cur as usize;
                if cur_usize >= n {
                    break;
                }
                let display = {
                    let raw_ci = g.class_idx.get(cur_usize).copied().unwrap_or(0) as usize;
                    if raw_ci < g.class_names.len() {
                        pretty_class_name(&g.class_names[raw_ci])
                    } else {
                        String::new()
                    }
                };
                let retained_val = g.retained.get(cur_usize).copied().unwrap_or(0);
                let root_label: Option<String> = root_type_of
                    .get(&cur)
                    .and_then(|&ty| gc_root_type_label_opt(ty).map(|l| l.to_string()));

                chain.push(RootPathStep {
                    obj_index_1based: cur_usize + 1,
                    display_class: display,
                    retained: retained_val,
                    root_type_label: root_label.clone(),
                    field_edge: None,
                });

                if root_label.is_some() {
                    break;
                }
                let parent = g.idom.get(cur_usize).copied().unwrap_or(undef_idom);
                if parent == undef_idom || parent == vroot || parent == cur {
                    break;
                }
                cur = parent;
            }
            if !chain.is_empty() {
                histogram[hist_pos].root_path = Some(chain);
            }
        }
    }

    // ── Boxed Numbers: filter histogram for Java boxed types ─────────────────
    const BOXED_TYPES: &[&str] = &[
        "java.lang.Integer",
        "java.lang.Long",
        "java.lang.Double",
        "java.lang.Boolean",
        "java.lang.Short",
        "java.lang.Byte",
        "java.lang.Character",
        "java.lang.Float",
        "java.lang.BigInteger",
        "java.lang.BigDecimal",
    ];
    let boxed_numbers: Vec<crate::report::BoxedNumberRow> = {
        let mut rows: Vec<crate::report::BoxedNumberRow> = histogram
            .iter()
            .filter(|r| BOXED_TYPES.contains(&r.pretty_class.as_str()))
            .map(|r| crate::report::BoxedNumberRow {
                pretty_class: r.pretty_class.clone(),
                instances: r.instances,
                total_shallow: r.shallow,
                pct_of_heap_bp: if total_shallow > 0 {
                    ((r.shallow as f64 / total_shallow as f64) * 10_000.0) as u32
                } else {
                    0
                },
                avg_shallow: r.shallow.checked_div(r.instances).unwrap_or(0),
            })
            .collect();
        rows.sort_unstable_by_key(|r| std::cmp::Reverse(r.total_shallow));
        rows
    };

    // ── Header Overhead: per-class header cost (12/16 B × instances) ─────────
    // compressed_oops is computed later from g.ref_size / g.id_size; derive here.
    let header_bytes: u8 = if g.ref_size < g.id_size { 12 } else { 16 };
    let header_overhead: Vec<crate::report::HeaderOverheadRow> = {
        const HEADER_FLOOR_BYTES: u64 = 1024 * 1024; // 1 MiB total header cost
        const HEADER_PCT_BP_FLOOR: u32 = 3_000; // 30% of shallow
        let mut rows: Vec<crate::report::HeaderOverheadRow> = histogram
            .iter()
            .filter(|r| r.instances > 0 && r.shallow > 0)
            .filter_map(|r| {
                let total_header = r.instances.saturating_mul(header_bytes as u64);
                let header_pct_bp = ((total_header as f64 / r.shallow as f64) * 10_000.0) as u32;
                if header_pct_bp >= HEADER_PCT_BP_FLOOR || total_header >= HEADER_FLOOR_BYTES {
                    Some(crate::report::HeaderOverheadRow {
                        pretty_class: r.pretty_class.clone(),
                        instances: r.instances,
                        header_bytes,
                        total_header_bytes: total_header,
                        header_pct_of_shallow_bp: header_pct_bp,
                        avg_shallow: r.shallow / r.instances,
                    })
                } else {
                    None
                }
            })
            .collect();
        rows.sort_unstable_by_key(|r| std::cmp::Reverse(r.total_header_bytes));
        rows.truncate(30);
        rows
    };

    // Per-class unreachable-objects histogram: capped, shallow-desc. Only
    // canonical rows (remap[ci] == ci) with unreachable objects are emitted.
    // Retained-within-the-forest (if computed by the unreachable_retained stage)
    // is looked up per canonical class row; 0 when the stage did not run.
    let unreach_retained_by_class = g
        .unreachable_retained
        .as_ref()
        .map(|u| &u.retained_by_class);
    // Fold the raw-class-keyed retained map through `remap` into canonical rows
    // once (the histogram merges duplicate class rows into their canonical ci).
    let unreach_retained_canonical: Vec<u64> = {
        let mut v = vec![0u64; class_count];
        if let Some(map) = unreach_retained_by_class {
            for (&rc, &r) in map {
                if (rc as usize) < class_count {
                    let ci = remap[rc as usize] as usize;
                    v[ci] += r;
                }
            }
        }
        v
    };
    let unreachable_histogram: Vec<UnreachableClassRow> = {
        let mut order: Vec<usize> = (0..class_count)
            .filter(|&ci| remap[ci] as usize == ci && unreach_count[ci] > 0)
            .collect();
        order.sort_unstable_by(|&a, &b| {
            unreach_shallow[b]
                .cmp(&unreach_shallow[a])
                .then(unreach_count[b].cmp(&unreach_count[a]))
                .then(a.cmp(&b))
        });
        order.truncate(UNREACHABLE_HISTOGRAM_CAP);
        order
            .into_iter()
            .map(|ci| UnreachableClassRow {
                pretty_class: pretty_class_name(&g.class_names[ci]),
                objects: unreach_count[ci],
                shallow: unreach_shallow[ci],
                retained: unreach_retained_canonical[ci],
            })
            .collect()
    };

    // F2: class-loader rollup + duplicate-class detection. Both are bounded
    // folds over `histogram` (one pass; maps keyed by loader_id / pretty_class,
    // so at most #loaders / #class-names entries — no per-object arrays).
    let (loader_rollup, duplicate_classes) = {
        use std::collections::HashMap;
        const LOADER_CAP: usize = 8;
        // Rollup: aggregate per loader_id.
        let mut roll: HashMap<u64, LoaderRollup> = HashMap::new();
        // Duplicate detection: per pretty_class, the distinct loader ids and
        // (labels, totals) seen. Labels de-duped in first-seen order.
        struct DupAcc {
            loader_ids: std::collections::HashSet<u64>,
            loaders: Vec<String>,
            total_instances: u64,
            total_retained: u64,
            // loader_id -> (label, instances, shallow, retained); capped at
            // LOADER_CAP entries (an existing entry always accumulates).
            per_loader: HashMap<u64, (String, u64, u64, u64)>,
        }
        let mut dup: HashMap<String, DupAcc> = HashMap::new();

        for row in &histogram {
            let e = roll.entry(row.loader_id).or_insert_with(|| LoaderRollup {
                loader_label: row.loader_label.clone(),
                loader_id: row.loader_id,
                class_count: 0,
                instances: 0,
                shallow: 0,
                retained: 0,
            });
            e.class_count += 1;
            e.instances += row.instances;
            e.shallow += row.shallow;
            e.retained += row.retained;

            let d = dup
                .entry(row.pretty_class.clone())
                .or_insert_with(|| DupAcc {
                    loader_ids: std::collections::HashSet::new(),
                    loaders: Vec::new(),
                    total_instances: 0,
                    total_retained: 0,
                    per_loader: HashMap::new(),
                });
            let label = row
                .loader_label
                .clone()
                .unwrap_or_else(|| format!("loader@{:#x}", row.loader_id));
            if d.loader_ids.insert(row.loader_id) && d.loaders.len() < LOADER_CAP {
                d.loaders.push(label.clone());
            }
            d.total_instances += row.instances;
            d.total_retained += row.retained;
            if d.per_loader.contains_key(&row.loader_id) || d.per_loader.len() < LOADER_CAP {
                let e = d
                    .per_loader
                    .entry(row.loader_id)
                    .or_insert((label, 0, 0, 0));
                e.1 += row.instances;
                e.2 += row.shallow;
                e.3 += row.retained;
            }
        }

        let mut rollup: Vec<LoaderRollup> = roll.into_values().collect();
        rollup.sort_unstable_by(|a, b| {
            b.retained
                .cmp(&a.retained)
                .then(a.loader_id.cmp(&b.loader_id))
        });
        rollup.truncate(top_n);

        let mut dups: Vec<DuplicateClass> = dup
            .into_iter()
            .filter(|(_, d)| d.loader_ids.len() > 1)
            .map(|(pretty_class, d)| {
                let DupAcc {
                    loader_ids,
                    loaders,
                    total_instances,
                    total_retained,
                    per_loader,
                } = d;
                let mut per_loader: Vec<DuplicateClassLoaderRow> = per_loader
                    .into_iter()
                    .map(
                        |(loader_id, (loader_label, instances, shallow, retained))| {
                            DuplicateClassLoaderRow {
                                loader_label,
                                loader_id,
                                instances,
                                shallow,
                                retained,
                            }
                        },
                    )
                    .collect();
                per_loader.sort_unstable_by(|a, b| {
                    b.retained
                        .cmp(&a.retained)
                        .then(b.instances.cmp(&a.instances))
                        .then(a.loader_id.cmp(&b.loader_id))
                });
                DuplicateClass {
                    pretty_class,
                    loader_count: loader_ids.len() as u64,
                    loaders,
                    total_instances,
                    total_retained,
                    per_loader,
                }
            })
            .collect();
        dups.sort_unstable_by(|a, b| {
            b.total_retained
                .cmp(&a.total_retained)
                .then_with(|| a.pretty_class.cmp(&b.pretty_class))
        });
        dups.truncate(top_n);
        (rollup, dups)
    };

    // Heap-shape scalars.
    let heap_fragmentation_ratio = compute_fragmentation_ratio(total_shallow, unreachable_shallow);
    let top_class_concentration_bp =
        compute_top_class_concentration_bp(&histogram, retention_concentration.total_retained);

    // GC roots retained by type: aggregate retained heap per root type.
    let gc_roots_retained_by_type: Vec<crate::report::GcRootRetainedRow> = {
        use std::collections::HashMap;
        // by_type: root_type_label → (count, retained, HashMap<class_name → (count, retained)>)
        type ByTypeMap = HashMap<String, (u64, u64, HashMap<String, (u64, u64)>)>;
        let mut by_type: ByTypeMap = HashMap::new();
        for (&idx, &ty) in g.gc_root_indices.iter().zip(g.gc_root_types.iter()) {
            if let Some(label) = gc_root_type_label_opt(ty) {
                let i = idx as usize;
                let retained = g.retained.get(i).copied().unwrap_or(0);
                let cls = class_display(g, i);
                let e = by_type.entry(label.to_string()).or_default();
                e.0 += 1;
                e.1 = e.1.saturating_add(retained);
                let ce = e.2.entry(cls).or_insert((0, 0));
                ce.0 += 1;
                ce.1 = ce.1.saturating_add(retained);
            }
        }
        let mut rows: Vec<crate::report::GcRootRetainedRow> = by_type
            .into_iter()
            .map(|(root_type, (count, retained, class_map))| {
                let mut top: Vec<crate::report::GcRootClassRow> = class_map
                    .into_iter()
                    .map(|(class_name, (c, r))| crate::report::GcRootClassRow {
                        class_name,
                        count: c,
                        retained: r,
                    })
                    .collect();
                top.sort_unstable_by(|a, b| {
                    b.retained
                        .cmp(&a.retained)
                        .then(a.class_name.cmp(&b.class_name))
                });
                top.truncate(5);
                crate::report::GcRootRetainedRow {
                    root_type,
                    count,
                    retained,
                    top_classes: top,
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            b.retained
                .cmp(&a.retained)
                .then(a.root_type.cmp(&b.root_type))
        });
        rows
    };

    // Compressed OOPs: references narrower than identifiers (id_size 8 -> ref 4).
    let compressed_oops = Some(g.ref_size < g.id_size);
    let dump_creation = if g.header_timestamp_ms != 0 {
        Some(g.header_timestamp_ms as i64)
    } else {
        None
    };

    let overview = SystemOverview {
        source_name: g.source_name.clone(),
        file_path: g.file_path.clone(),
        format: g.format.clone(),
        file_size: g.file_size,
        identifier_size_bits: g.id_size as u32 * 8,
        compressed_oops,
        dump_creation,
        total_objects,
        total_shallow,
        gc_roots,
        gc_roots_by_type,
        heap_composition,
        dominator_depth_histogram,
        retention_concentration,
        classes_loaded,
        classloaders_loaded,
        unreachable_count,
        unreachable_shallow,
        unreachable_retained: g
            .unreachable_retained
            .as_ref()
            .map(|u| u.total)
            .unwrap_or(0),
        unreachable_composition,
        unreachable_garbage_roots: g
            .unreachable_retained
            .as_ref()
            .map(|u| u.garbage_roots.clone())
            .unwrap_or_default(),
        unreachable_histogram,
        histogram,
        histogram_truncated_to: None,
        system_properties: g
            .system_properties
            .iter()
            .map(|(k, v)| PropEntry {
                key: k.clone(),
                value: v.clone(),
            })
            .collect(),
        jvm_version: g.jvm_version.clone(),
        loader_rollup,
        duplicate_classes,
        record_census: g.record_census.clone(),
        duplicate_strings: g.dup_strings.clone(),
        duplicate_prim_arrays: g.dup_prim_arrays.clone(),
        boxed_numbers,
        header_overhead,
        boxed_number_holders: g.boxed_number_holders.clone(),
        heap_fragmentation_ratio,
        top_class_concentration_bp,
        gc_roots_retained_by_type,
    };
    (overview, top_level_list)
}

/// Build the FULL multi-level dominator subtree rooted at `root`, walking the
/// dominator-children CSR via `dom_children`. Children at each node are sorted
/// retained-desc (tie: obj index asc) — the SAME comparator as the capped
/// `dominated` list — and expanded heaviest-first so that when the global
/// node budget (`max_nodes`) is exhausted the heaviest subtrees are retained.
/// Descent stops at `max_depth` (root is depth 0; a node AT `max_depth` keeps
/// an empty `children`). No cycle guard is needed (a dominator tree is a tree),
/// but both caps are enforced. Uses an explicit stack — never recurses — so it
/// is safe on deep trees.
/// Build the FULL multi-level dominator subtree rooted at `root`,
/// using an explicit-stack iterative post-order walk over the dominator-children
/// CSR (no recursion, so a deep tree cannot blow the native stack). Children are
/// sorted retained-desc (tie: obj idx asc) and the walk is bounded by both a
/// `max_nodes` cap (total emitted nodes) and a `max_depth` cap so the subtree
/// stays small regardless of heap shape.
fn build_dom_subtree(
    root: usize,
    dc_offsets: &[u32],
    dc_targets: &[u32],
    display_of: &dyn Fn(usize) -> String,
    g: &Graph,
    max_nodes: usize,
    max_depth: usize,
) -> DomTreeNode {
    // A partially-built node plus the retained-desc-sorted queue of its child
    // node indices that still need to be visited (cursor at `child_pos`).
    struct Frame {
        depth: usize,
        node: DomTreeNode,
        pending: Vec<u32>,
        child_pos: usize,
    }

    // Sort a node's dominator children retained-desc, tie-break obj idx asc.
    let sorted_children = |idx: usize| -> Vec<u32> {
        let mut kids: Vec<u32> =
            dc_targets[dc_offsets[idx] as usize..dc_offsets[idx + 1] as usize].to_vec();
        kids.sort_unstable_by(|&a, &b| {
            g.retained[b as usize]
                .cmp(&g.retained[a as usize])
                .then(a.cmp(&b))
        });
        kids
    };

    let make_node = |idx: usize| DomTreeNode {
        obj_index_1based: idx + 1,
        display_class: display_of(idx),
        shallow: g.shallow[idx] as u64,
        retained: g.retained[idx],
        children: Vec::new(),
    };

    // Root counts as node 1. `max_nodes == 0` is treated as "at least the root".
    let mut emitted: usize = 1;

    // If the root is already at the depth cap there are no children to expand.
    let root_pending = if max_depth == 0 {
        Vec::new()
    } else {
        sorted_children(root)
    };
    let mut stack: Vec<Frame> = vec![Frame {
        depth: 0,
        node: make_node(root),
        pending: root_pending,
        child_pos: 0,
    }];

    // Iterative post-order: advance the top frame's cursor; when a child is
    // admitted push a new frame; when a frame's children are exhausted pop it
    // and splice its finished node into its parent's `children`.
    loop {
        let top = stack.last_mut().expect("stack never empties before break");
        let can_descend = top.depth < max_depth;
        if can_descend && top.child_pos < top.pending.len() && emitted < max_nodes {
            let child = top.pending[top.child_pos] as usize;
            top.child_pos += 1;
            emitted += 1;
            let depth = top.depth + 1;
            let pending = if depth < max_depth {
                sorted_children(child)
            } else {
                Vec::new()
            };
            stack.push(Frame {
                depth,
                node: make_node(child),
                pending,
                child_pos: 0,
            });
        } else {
            // This frame is done (depth/node cap hit or children exhausted).
            let done = stack.pop().expect("frame present").node;
            match stack.last_mut() {
                Some(parent) => parent.node.children.push(done),
                None => return done,
            }
        }
    }
}

/// Build the "Leak Suspects" model: single top-level dominators and class
/// groups whose retained heap exceeds `THRESHOLD_PCT` of the total, each with
/// its accumulation-point descent and bounded dominated-children detail. Always
/// walks the dominator chain (via `idom`) from each single suspect up to its GC
/// root (bounded by `root_path_max_depth`) and attaches the full dominator
/// subtree (bounded by `dom_max_nodes`/`dom_max_depth`).
pub(crate) fn build_leak_suspects(
    g: &Graph,
    dc_offsets: &[u32],
    dc_targets: &[u32],
    cap: usize,
    root_path_max_depth: usize,
    dom_max_nodes: usize,
    dom_max_depth: usize,
) -> LeakSuspects {
    let n = g.n;
    let undef = u32::MAX;

    // Total shallow heap of reachable objects
    let mut total_shallow: u64 = (0..n)
        .filter(|&i| g.idom[i] != undef)
        .map(|i| g.shallow[i] as u64)
        .sum();
    // Include MAT's synthetic <system class loader> object for internal
    // consistency with build_system_overview's total_shallow.
    if let Some(sz) = g.system_classloader_shallow {
        total_shallow += sz as u64;
    }

    let threshold = (total_shallow as f64 * THRESHOLD_PCT / 100.0) as u64;

    // The dominator-children CSR (dc_offsets/dc_targets) is built ONCE in main
    // by retained::build_dom_children_csr and shared with compute_retained.
    let dom_children = |node: usize| -> &[u32] {
        &dc_targets[dc_offsets[node] as usize..dc_offsets[node + 1] as usize]
    };

    struct RawSuspect {
        is_single: bool,
        obj_idx: u32, // only meaningful for single
        class_idx: usize,
        instance_count: u64,
        retained: u64,
        shallow: u64,
    }

    let mut suspects: Vec<RawSuspect> = Vec::new();
    let mut single_class_set: std::collections::HashSet<usize> = std::collections::HashSet::new();

    // Phase 1: single objects directly dominated by vroot with retained >= threshold
    for &i in dom_children(n) {
        let idx = i as usize;
        if g.retained[idx] >= threshold {
            let ci = g.class_idx[idx] as usize;
            single_class_set.insert(ci);
            suspects.push(RawSuspect {
                is_single: true,
                obj_idx: i,
                class_idx: ci,
                instance_count: 1,
                retained: g.retained[idx],
                shallow: g.shallow[idx] as u64,
            });
        }
    }

    // Phase 2: class groups of top-level dominators
    let class_count = g.class_names.len();
    let mut group_retained: Vec<u64> = vec![0; class_count];
    let mut group_count: Vec<u64> = vec![0; class_count];
    let mut group_shallow: Vec<u64> = vec![0; class_count];
    for &i in dom_children(n) {
        let idx = i as usize;
        let ci = g.class_idx[idx] as usize;
        if ci < class_count {
            group_retained[ci] += g.retained[idx];
            group_count[ci] += 1;
            group_shallow[ci] += g.shallow[idx] as u64;
        }
    }
    for ci in 0..class_count {
        if group_retained[ci] >= threshold && !single_class_set.contains(&ci) {
            suspects.push(RawSuspect {
                is_single: false,
                obj_idx: u32::MAX,
                class_idx: ci,
                instance_count: group_count[ci],
                retained: group_retained[ci],
                shallow: group_shallow[ci],
            });
        }
    }

    // Sort by retained desc, with explicit tie-breaker on (class_idx asc,
    // obj_idx asc) so equal-retained suspects are deterministic.
    suspects.sort_unstable_by(|a, b| {
        b.retained
            .cmp(&a.retained)
            .then(a.class_idx.cmp(&b.class_idx))
            .then(a.obj_idx.cmp(&b.obj_idx))
    });

    // For class objects, show the class they represent (MAT parity: no
    // "class " prefix); otherwise the object's own class.
    let display_of = |idx: usize| -> String {
        let ci = g.class_idx[idx] as usize;
        if class_obj_repr(g, idx) != u32::MAX {
            let repr = class_obj_repr(g, idx) as usize;
            if repr < g.class_names.len() {
                return pretty_class_name(&g.class_names[repr]);
            }
        }
        if ci < g.class_names.len() {
            pretty_class_name(&g.class_names[ci])
        } else {
            String::from("?")
        }
    };

    // Map each root object index -> a representative root type. When one index
    // carries several root records we keep the minimum sub-tag (deterministic),
    // matching the representative-type convention documented on
    // `Graph::gc_root_types`. Suspects are top-level dominators (idom == vroot),
    // so the only single root that can hold one is the object itself; we resolve
    // the holding root TYPE by looking the suspect's object up in this map.
    let mut root_type_of: std::collections::HashMap<u32, u8> = std::collections::HashMap::new();
    for (idx, &ty) in g.gc_root_indices.iter().zip(g.gc_root_types.iter()) {
        root_type_of
            .entry(*idx)
            .and_modify(|e| *e = (*e).min(ty))
            .or_insert(ty);
    }

    // Build the "merged shortest paths to GC roots" prefix tree for a class-group
    // suspect (Eclipse MAT "Merge Shortest Paths"). Each member's dominator chain
    // (member -> idom -> ... -> GC root, the SAME walk the single-suspect
    // `root_path` loop performs) is grafted beneath a synthetic virtual root
    // summarising the group. Chains are merged by the DISPLAYED class label at
    // each depth: two hops with the same displayed class collapse into one node.
    // Keying by the displayed class (rather than a numeric class row) matches
    // MAT's class-keyed merge and sidesteps class-mirror/remap edge cases.
    //
    // Implemented as a CLOSURE (not a free fn) so it can borrow the local
    // `display_of` / `root_type_of` closures and `g` directly — a free fn would
    // have to thread all of them through. The closure borrows `g`, `display_of`,
    // `root_type_of` immutably; the group loop below mutates `out` (a disjoint
    // binding), so the borrow checker is satisfied.
    //
    // The tree is assembled in a flat arena (indices, not references) to avoid
    // recursive borrows; find-or-create scans a node's `children` for a matching
    // label, so iterating members in a deterministic (ascending index) order
    // makes insertion order — and therefore the result — deterministic.

    // Helper: look up the field name on `parent` that references `child`.
    // Returns `None` when --ref-paths was not set or no matching named edge exists.
    let field_name_for = |parent: usize, child: usize| -> Option<String> {
        let pool = g.field_name_pool.as_ref()?;
        let idx_vec = g.fwd_field_name_idx.as_ref()?;
        let start = g.fwd_offsets[parent] as usize;
        let end = g.fwd_offsets[parent + 1] as usize;
        for pos in start..end {
            if g.fwd_targets.get(pos) as usize == child {
                let name_idx = idx_vec[pos] as usize;
                if name_idx < pool.len() && !pool[name_idx].is_empty() {
                    return Some(pool[name_idx].clone());
                }
                return None;
            }
        }
        None
    };

    let vroot_u32 = n as u32;
    let build_merged_paths = |members: &[u32], group_label: &str| -> Option<MergedPathNode> {
        if members.is_empty() {
            return None;
        }
        struct MNode {
            display_class: String,
            object_count: u64,
            retained: u64,
            root_type_label: Option<String>,
            /// The field on the parent that points here. `None` when not all
            /// member chains agree on a single field name, or --ref-paths off.
            field_edge: Option<String>,
            children: Vec<usize>,
        }
        let mut arena: Vec<MNode> = Vec::new();
        // Synthetic virtual root summarising the whole group. Its label is the
        // group's own class; counts/retained accumulate as members are grafted.
        arena.push(MNode {
            display_class: group_label.to_string(),
            object_count: 0,
            retained: 0,
            root_type_label: None,
            field_edge: None,
            children: Vec::new(),
        });

        for &m in members {
            // Fast path: if this member's immediate dominator is vroot (or undef),
            // the chain is exactly [m] (depth 1). For non-class-object members the
            // display label equals group_label (all members share the same class),
            // so skip the per-member display_of call and reuse group_label directly.
            let m_idom = g.idom[m as usize];
            if m_idom == vroot_u32 || m_idom == undef {
                let ret = g.retained[m as usize];
                arena[0].object_count += 1;
                arena[0].retained += ret;
                // For class objects, display_of returns the represented-class name
                // (varies per member); fall through to label computation. For all
                // others, display_of == group_label (same class ⟹ same pretty name).
                let label: std::borrow::Cow<str> = if class_obj_repr(g, m as usize) == u32::MAX {
                    std::borrow::Cow::Borrowed(group_label)
                } else {
                    std::borrow::Cow::Owned(display_of(m as usize))
                };
                let existing = arena[0]
                    .children
                    .iter()
                    .copied()
                    .find(|&c| arena[c].display_class == label.as_ref());
                let child = match existing {
                    Some(c) => c,
                    None => {
                        if arena.len() >= MERGED_PATH_MAX_NODES {
                            continue;
                        }
                        let idx = arena.len();
                        arena.push(MNode {
                            display_class: label.into_owned(),
                            object_count: 0,
                            retained: 0,
                            root_type_label: None,
                            field_edge: None,
                            children: Vec::new(),
                        });
                        arena[0].children.push(idx);
                        idx
                    }
                };
                arena[child].object_count += 1;
                arena[child].retained += ret;
                if arena[child].root_type_label.is_none() {
                    if let Some(&ty) = root_type_of.get(&m) {
                        if let Some(lbl) = gc_root_type_label_opt(ty) {
                            arena[child].root_type_label = Some(lbl.to_string());
                        }
                    }
                }
                continue;
            }
            // General path: multi-hop chain walk for members not directly under vroot.
            let mut chain: Vec<usize> = Vec::new();
            let mut cur = m as usize;
            let mut depth = 0usize;
            loop {
                chain.push(cur);
                let idom = g.idom[cur];
                if idom == vroot_u32 || idom == undef {
                    break;
                }
                if depth >= root_path_max_depth {
                    break;
                }
                cur = idom as usize;
                depth += 1;
            }
            let last = chain.len().saturating_sub(1);
            // Graft the chain beneath the virtual root, merging by display label.
            let mut node = 0usize; // virtual root
            arena[node].object_count += 1;
            arena[node].retained += g.retained[m as usize];
            for (hop_i, &obj) in chain.iter().enumerate() {
                let label = display_of(obj);
                // The field edge on this node is the field on chain[hop_i+1]
                // (the parent, one hop closer to root) that references obj.
                let this_field_edge = if hop_i + 1 < chain.len() {
                    field_name_for(chain[hop_i + 1], obj)
                } else {
                    None
                };
                // find-or-create a child of `node` with this label.
                let existing = arena[node]
                    .children
                    .iter()
                    .copied()
                    .find(|&c| arena[c].display_class == label);
                let child = match existing {
                    Some(c) => {
                        // Clear field_edge if chains disagree.
                        if arena[c].field_edge.as_deref() != this_field_edge.as_deref() {
                            arena[c].field_edge = None;
                        }
                        c
                    }
                    None => {
                        // Node cap: stop creating NEW nodes once reached, but keep
                        // accumulating into existing matching nodes above.
                        if arena.len() >= MERGED_PATH_MAX_NODES {
                            break;
                        }
                        let idx = arena.len();
                        arena.push(MNode {
                            display_class: label,
                            object_count: 0,
                            retained: 0,
                            root_type_label: None,
                            field_edge: this_field_edge,
                            children: Vec::new(),
                        });
                        arena[node].children.push(idx);
                        idx
                    }
                };
                arena[child].object_count += 1;
                arena[child].retained += g.retained[obj];
                // The terminal hop is the GC root; label it if not already set.
                if hop_i == last && arena[child].root_type_label.is_none() {
                    if let Some(&ty) = root_type_of.get(&(obj as u32)) {
                        if let Some(lbl) = gc_root_type_label_opt(ty) {
                            arena[child].root_type_label = Some(lbl.to_string());
                        }
                    }
                }
                node = child;
            }
        }

        // Deterministic ordering: each node's children by retained desc, then
        // object_count desc, then display_class asc.
        for i in 0..arena.len() {
            let mut kids = std::mem::take(&mut arena[i].children);
            kids.sort_by(|&a, &b| {
                arena[b]
                    .retained
                    .cmp(&arena[a].retained)
                    .then(arena[b].object_count.cmp(&arena[a].object_count))
                    .then(arena[a].display_class.cmp(&arena[b].display_class))
            });
            arena[i].children = kids;
        }

        // Convert the arena into the nested model. Depth is bounded by
        // `root_path_max_depth`, so bounded recursion is safe here.
        fn to_model(arena: &[MNode], idx: usize) -> MergedPathNode {
            let node = &arena[idx];
            MergedPathNode {
                display_class: node.display_class.clone(),
                object_count: node.object_count,
                retained: node.retained,
                root_type_label: node.root_type_label.clone(),
                field_edge: node.field_edge.clone(),
                children: node.children.iter().map(|&c| to_model(arena, c)).collect(),
            }
        }
        Some(to_model(&arena, 0))
    };

    // Materialise into the model, resolving the accumulation point for singles
    // via MAT's findAccumulationPoint (big-drop-ratio descent) and the holding
    // GC-root type.
    let mut out: Vec<Suspect> = suspects
        .iter()
        .map(|s| {
            let mut path: Vec<PathStep> = Vec::new();
            let mut accumulation: Option<usize> = None;
            let mut root_type_label = String::new();
            if s.is_single {
                // The suspect object is a top-level dominator; if it is itself a
                // GC root of an identifiable type, that root type holds it.
                if let Some(&ty) = root_type_of.get(&s.obj_idx) {
                    if let Some(label) = gc_root_type_label_opt(ty) {
                        root_type_label = label.to_string();
                    }
                }
                // Descend the dominator tree to the largest-retained child while
                // that child retains >= BIG_DROP_RATIO of its parent; the parent
                // at the first big drop (or a leaf) is the accumulation point.
                let mut cur = s.obj_idx as usize;
                let mut cur_ret = g.retained[cur];
                path.push(PathStep {
                    depth: 0,
                    obj_index_1based: cur + 1,
                    display_class: display_of(cur),
                    retained: cur_ret,
                });
                let mut depth = 0usize;
                loop {
                    let best_child = dom_children(cur).iter().max_by(|&&a, &&b| {
                        g.retained[a as usize]
                            .cmp(&g.retained[b as usize])
                            .then(b.cmp(&a))
                    });
                    let Some(&c) = best_child else {
                        // Leaf: current object is the accumulation point.
                        accumulation = Some(cur);
                        break;
                    };
                    let child = c as usize;
                    let child_ret = g.retained[child];
                    let drops = (child_ret as f64) < (cur_ret as f64) * BIG_DROP_RATIO;
                    if drops {
                        // Big drop: parent is the accumulation point; do not
                        // descend into the child.
                        accumulation = Some(cur);
                        break;
                    }
                    depth += 1;
                    if depth >= MAX_ACCUM_DEPTH {
                        // No big drop within MAX_DEPTH: no accumulation point.
                        break;
                    }
                    path.push(PathStep {
                        depth,
                        obj_index_1based: child + 1,
                        display_class: display_of(child),
                        retained: child_ret,
                    });
                    cur = child;
                    cur_ret = child_ret;
                }
            }

            // Accumulated objects: the accumulation point's immediately
            // dominated children (retained-desc, tie obj-idx asc), capped.
            let mut dominated: Vec<DominatedRow> = Vec::new();
            let mut dominated_by_class: Vec<HistRow> = Vec::new();
            let mut dominated_total_count: u64 = 0;
            if let Some(ap) = accumulation {
                let mut kids: Vec<u32> = dom_children(ap).to_vec();
                dominated_total_count = kids.len() as u64;
                kids.sort_unstable_by(|&a, &b| {
                    g.retained[b as usize]
                        .cmp(&g.retained[a as usize])
                        .then(a.cmp(&b))
                });
                for &k in kids.iter().take(cap) {
                    let ki = k as usize;
                    dominated.push(DominatedRow {
                        obj_index_1based: ki + 1,
                        display_class: display_of(ki),
                        shallow: g.shallow[ki] as u64,
                        retained: g.retained[ki],
                    });
                }
                // By-class histogram of ALL immediately-dominated children.
                let class_count = g.class_names.len();
                let mut cls_count: std::collections::HashMap<usize, (u64, u64, u64)> =
                    std::collections::HashMap::new();
                for &k in &kids {
                    let ki = k as usize;
                    let ci = g.class_idx[ki] as usize;
                    if ci < class_count {
                        let e = cls_count.entry(ci).or_insert((0, 0, 0));
                        e.0 += 1;
                        e.1 += g.shallow[ki] as u64;
                        e.2 += g.retained[ki];
                    }
                }
                let mut rows: Vec<(usize, u64, u64, u64)> = cls_count
                    .into_iter()
                    .map(|(ci, (c, sh, ret))| (ci, c, sh, ret))
                    .collect();
                rows.sort_unstable_by(|a, b| b.3.cmp(&a.3).then(a.0.cmp(&b.0)));
                for (ci, c, sh, ret) in rows.into_iter().take(cap) {
                    dominated_by_class.push(HistRow {
                        pretty_class: pretty_class_name(&g.class_names[ci]),
                        instances: c,
                        shallow: sh,
                        retained: ret,
                        max_instance_shallow: 0,
                        incoming_ref_count: 0,
                        loader_id: g.class_loader_id.get(ci).copied().unwrap_or(0),
                        loader_label: {
                            // `ci` = g.class_idx[ki], a valid histogram row
                            // index aligned with class_loader_id.
                            let lid = g.class_loader_id.get(ci).copied().unwrap_or(0);
                            if lid == 0 {
                                Some("<boot>".to_string())
                            } else {
                                g.loader_labels.get(&lid).cloned()
                            }
                        },
                        root_path: None,
                    });
                }
            }

            // Keywords: suspect class + accumulation-point class, first-seen order.
            // For a single suspect whose object is itself a class mirror, resolve
            // the REPRESENTED class (via display_of) so we print e.g.
            // `scala.runtime.LazyVals$` not `java.lang.Class` (MAT parity). Group
            // suspects have no object (obj_idx == u32::MAX) so use their class row.
            let pretty_class = if s.obj_idx != u32::MAX {
                display_of(s.obj_idx as usize)
            } else {
                pretty_class_name(&g.class_names[s.class_idx])
            };
            let mut keywords: Vec<String> = vec![pretty_class.clone()];
            let (accumulation_class, accumulation_retained, accumulation_obj_1based) =
                match accumulation {
                    Some(ap) => {
                        let ac = display_of(ap);
                        if !keywords.contains(&ac) {
                            keywords.push(ac.clone());
                        }
                        (Some(ac), Some(g.retained[ap]), Some(ap + 1))
                    }
                    None => (None, None, None),
                };

            let dominated_len_captured = dominated.len() as u64;

            // Build the FULL multi-level dominator subtree rooted at the
            // accumulation point, bounded by dom_max_nodes / dom_max_depth.
            // Explicit-stack walk to avoid recursion blowing the stack on deep
            // trees; heaviest children are expanded first so a node-cap cutoff
            // keeps the largest subtrees.
            let dominator_tree_node: Option<DomTreeNode> = accumulation.map(|ap| {
                build_dom_subtree(
                    ap,
                    dc_offsets,
                    dc_targets,
                    &display_of,
                    g,
                    dom_max_nodes,
                    dom_max_depth,
                )
            });

            Suspect {
                is_single: s.is_single,
                pretty_class,
                instance_count: s.instance_count,
                retained: s.retained,
                shallow: s.shallow,
                path,
                accumulation_obj_1based,
                accumulation_class,
                accumulation_retained,
                dominated,
                dominated_total_count,
                dominated_shown: dominated_len_captured,
                dominated_by_class,
                keywords,
                root_type_label,
                root_path: None,
                dominator_tree: dominator_tree_node,
                merged_paths: None,
            }
        })
        .collect();

    // For each SINGLE suspect, walk the DOMINATOR chain from the
    // suspect object up toward the GC root, emitting a bounded reference chain.
    // For each SINGLE suspect, walk dominator chain from suspect object toward GC
    // root (bounded by `root_path_max_depth`) and attaches the full dominator
    // chain. This mirrors MAT's Leak Suspects "path to the accumulation point", which is
    // itself dominator-based: `idom[node]` is the object that must be released for
    // `node` to become collectable, so the chain suspect -> idom -> ... -> root is
    // exactly "what is keeping this alive". It reuses the already-resident `idom`
    // array (no inbound-CSR preservation, no decompression, no extra RSS).
    {
        let vroot = n as u32;
        for (k, s) in suspects.iter().enumerate() {
            if !s.is_single {
                continue;
            }
            let mut chain: Vec<RootPathStep> = Vec::new();
            let mut cur = s.obj_idx as usize;
            let mut depth = 0usize;
            loop {
                // A node dominated directly by the virtual root (idom == vroot) is
                // a GC root; label it and terminate. `undef` guards unreachable
                // nodes (should not occur for a reachable suspect, but is cheap).
                let idom = g.idom[cur];
                let is_root = idom == vroot;
                let root_type_label = root_type_of
                    .get(&(cur as u32))
                    .and_then(|&ty| gc_root_type_label_opt(ty).map(|l| l.to_string()));
                // The field_edge for step i is the field on the NEXT hop (idom[cur])
                // that references cur. We look it up here, before advancing cur.
                let field_edge = if !is_root && idom != undef {
                    field_name_for(idom as usize, cur)
                } else {
                    None
                };
                chain.push(RootPathStep {
                    obj_index_1based: cur + 1,
                    display_class: display_of(cur),
                    retained: g.retained[cur],
                    root_type_label,
                    field_edge,
                });
                if is_root || idom == undef {
                    break;
                }
                if depth >= root_path_max_depth {
                    break;
                }
                cur = idom as usize;
                depth += 1;
            }
            out[k].root_path = Some(chain);
        }
    }

    // For each GROUP suspect, build the merged shortest-paths-to-GC-roots prefix
    // tree (symmetric to the single-suspect `root_path` loop above). Members are
    // the top-level dominators (children of vroot) whose class row matches the
    // group's class — the same member enumeration `dom_children(n)` already used
    // twice in this fn, filtered by class. Sorted ascending for determinism.
    {
        for (k, s) in suspects.iter().enumerate() {
            if s.is_single {
                continue;
            }
            let mut members: Vec<u32> = dom_children(n)
                .iter()
                .copied()
                .filter(|&i| g.class_idx[i as usize] as usize == s.class_idx)
                .collect();
            members.sort_unstable();
            let group_label = out[k].pretty_class.clone();
            out[k].merged_paths = build_merged_paths(&members, &group_label);
        }
    }

    LeakSuspects {
        total_shallow,
        suspects: out,
    }
}

/// Bucket a DESC-sorted slice of retained sizes into a power-of-two size
/// distribution plus basic stats. Additive; not parity-compared.
#[allow(dead_code)]
pub(crate) fn build_size_distribution(retained_desc: &[u64]) -> TopSizeDistribution {
    if retained_desc.is_empty() {
        return TopSizeDistribution::default();
    }
    let count = retained_desc.len() as u64;
    // sorted DESC, so max is first, min is last.
    let max = retained_desc[0];
    let min = *retained_desc.last().unwrap();
    let total: u64 = retained_desc.iter().sum();
    // Median of a DESC-sorted slice: middle element (lower-median for even n,
    // deterministic).
    let median = retained_desc[retained_desc.len() / 2];
    // Power-of-two buckets: bucket key = next_power_of_two(r).max(1). Aggregate
    // counts into a BTreeMap so buckets come out ascending & deterministic.
    let mut map: std::collections::BTreeMap<u64, u64> = std::collections::BTreeMap::new();
    for &r in retained_desc {
        // `next_power_of_two` panics (debug) / wraps to 0 (release) for r > 2^63;
        // clamp such absurd values (physically unreachable — an ~8 EiB single
        // dominator) into the top bucket rather than corrupting the histogram.
        let upper = if r <= 1 {
            1
        } else {
            r.checked_next_power_of_two().unwrap_or(u64::MAX)
        };
        *map.entry(upper).or_insert(0) += 1;
    }
    let buckets = map
        .into_iter()
        .map(|(upper_bytes, count)| SizeBucket { upper_bytes, count })
        .collect();
    TopSizeDistribution {
        buckets,
        count,
        min,
        max,
        median,
        total,
    }
}

/// Build a dense-object-index → "ClassName#methodName()" map for every object
/// that appears as a significant local in any thread's stack frames. Used to
/// annotate `ObjRow::held_via` for objects held by stack frames rather than
/// fields. Returns an empty map when `--thread-locals` was not set (the gated
/// `thread_local_frame_samples` map is empty).
fn build_stack_held_via(g: &Graph) -> std::collections::HashMap<u32, String> {
    use std::collections::HashMap;
    let mut out: HashMap<u32, String> = HashMap::new();
    for (&_thread_serial, pairs) in &g.thread_local_frame_samples {
        // Find the ThreadStack for this thread so we can resolve frame_number.
        let stack = g
            .thread_stacks
            .iter()
            .find(|ts| pairs.iter().any(|_| ts.thread_serial == _thread_serial));
        for &(frame_number, local_idx) in pairs {
            if out.contains_key(&local_idx) {
                continue; // first writer wins — highest-retained frame
            }
            let label = if let Some(ts) = stack {
                if frame_number == u32::MAX {
                    "<no frame>".to_string()
                } else {
                    ts.frames
                        .get(frame_number as usize)
                        .cloned()
                        .unwrap_or_else(|| format!("<frame #{frame_number}>"))
                }
            } else {
                format!("<frame #{frame_number}>")
            };
            // Convert the pre-rendered "class.method (source:line)" label to
            // "ClassName#methodName()" for the held-via column.
            out.insert(local_idx, frame_to_class_method(&label));
        }
    }
    out
}

/// Convert a pre-rendered frame line `"com.example.Foo.bar (Foo.java:42)"` to
/// `"com.example.Foo#bar()"`. Falls back to the original string on any parse
/// failure so the column always has a non-empty value.
fn frame_to_class_method(frame: &str) -> String {
    // Frame format: "class.method (source:line)" — strip the " (…)" suffix first.
    let label = if let Some(paren) = frame.find(" (") {
        &frame[..paren]
    } else {
        frame
    };
    // Split at the LAST dot to separate class from method.
    if let Some(dot) = label.rfind('.') {
        let class = &label[..dot];
        let method = &label[dot + 1..];
        if !class.is_empty() && !method.is_empty() {
            return format!("{}#{}()", class, method);
        }
    }
    frame.to_string()
}

/// Build the merged Top Retainers table (§813): combine `fields_by_size` rows
/// (Class#field retainers) and significant thread frames, deduplicated and
/// sorted by retained desc. Capped at 25 rows. Returns an empty Vec when both
/// sources are absent.
fn build_top_retainers(
    fields_by_size: &Option<FieldsBySize>,
    threads: &ThreadOverview,
) -> Vec<RetainerRow> {
    use std::collections::HashMap;
    let mut by_name: HashMap<String, (String, u64)> = HashMap::new(); // name -> (kind, retained)

    // Source 1: Class#field attribution rows.
    if let Some(fbs) = fields_by_size {
        for row in &fbs.rows {
            let name = format!("{}#{}", row.holder_class, row.field);
            let entry = by_name.entry(name).or_insert(("field".to_string(), 0));
            entry.1 = entry.1.saturating_add(row.total_retained);
        }
    }

    // Source 2: significant stack frames from thread overview.
    for thread in &threads.threads {
        for sf in &thread.significant_frames {
            if sf.frame.starts_with('<') {
                continue; // skip synthetic no-frame bucket
            }
            let label = frame_to_class_method(&sf.frame);
            let frame_retained: u64 = sf.locals.iter().map(|l| l.retained).sum();
            if frame_retained == 0 {
                continue;
            }
            let entry = by_name
                .entry(label)
                .or_insert(("stack-frame".to_string(), 0));
            entry.1 = entry.1.saturating_add(frame_retained);
        }
    }

    let mut rows: Vec<RetainerRow> = by_name
        .into_iter()
        .map(|(name, (kind, retained))| RetainerRow {
            name,
            kind,
            retained,
        })
        .collect();
    // Retained desc; tie-break name asc for determinism.
    rows.sort_by(|a, b| b.retained.cmp(&a.retained).then(a.name.cmp(&b.name)));
    rows.truncate(25);
    rows
}

/// Build the "Top Consumers" model: biggest objects (top-level dominators by
/// retained), biggest classes, and the pruned package tree. Bounded reductions
/// over the graph; no per-object Vec is retained.
fn build_top_consumers(
    g: &Graph,
    top_n: usize,
    stack_held_via: &std::collections::HashMap<u32, String>,
    mut top_level: Vec<u32>,
) -> TopConsumers {
    let n = g.n;
    let undef = u32::MAX;
    let class_count = g.class_names.len();

    // Sub-step timing (HPROF_TIMING-gated, stderr-only, byte-exact): splits the
    // build_top_consumers phase across its distinct passes so we can pick the
    // real hot sub-part rather than optimizing blind.
    #[cfg(not(target_family = "wasm"))]
    let _t_tc = std::time::Instant::now();
    macro_rules! t_tc {
        ($label:expr) => {
            #[cfg(not(target_family = "wasm"))]
            if std::env::var_os("HPROF_TIMING").is_some() {
                eprintln!(
                    "[timing] top_consumers/{}: {:.3}s",
                    $label,
                    _t_tc.elapsed().as_secs_f64()
                );
            }
        };
    }

    // top_level is pre-computed by build_system_overview's fused loop, eliminating
    // a redundant O(n) scan over g.idom.
    t_tc!("collect_top_level");

    // Total shallow of all reachable objects (MAT parity: pct base for Biggest Objects)
    let total_shallow: u64 = (0..n)
        .filter(|&i| g.idom[i] != undef)
        .map(|i| g.shallow[i] as u64)
        .sum();
    t_tc!("total_shallow");

    // Biggest Classes by Retained Heap (aggregate before sorting top_level)
    let mut class_retained: Vec<u64> = vec![0; class_count];
    let mut class_count_map: Vec<u64> = vec![0; class_count];
    // Fold duplicate `java/lang/Class` rows into the canonical row (see
    // `class_row_remap`) so the by-type count matches the histogram + MAT.
    let remap = class_row_remap(g);
    for &i in &top_level {
        let idx = i as usize;
        let ci = g.class_idx[idx] as usize;
        if ci < class_count {
            let ci = remap[ci] as usize;
            class_retained[ci] += g.retained[idx];
            class_count_map[ci] += 1;
        }
    }
    let mut class_order: Vec<usize> = (0..class_count)
        .filter(|&ci| class_retained[ci] > 0)
        .collect();
    // Retained desc, tie-breaker ascending class index.
    class_order
        .sort_unstable_by(|&a, &b| class_retained[b].cmp(&class_retained[a]).then(a.cmp(&b)));
    let biggest_classes: Vec<ClassRow> = class_order
        .iter()
        .take(top_n)
        .map(|&ci| ClassRow {
            pretty_class: pretty_class_name(&g.class_names[ci]),
            instances: class_count_map[ci],
            retained: class_retained[ci],
        })
        .collect();
    drop(class_retained);
    drop(class_count_map);
    drop(class_order);
    t_tc!("biggest_classes");

    // Biggest Packages: build a pruned package TREE (MAT PackageTreeResult
    // parity). Accumulate cumulative retained/shallow/count into a tree keyed by
    // an INTERNED segment id (u32), not the segment String. Each distinct package
    // segment (e.g. "java", "util") is interned once into `seg_names`; the hot
    // per-dominator loop then does zero String allocation (the old
    // `entry(seg.to_string())` allocated a fresh key on EVERY segment of EVERY
    // top-level dominator, even on BTreeMap hits — tens of millions of wasted
    // allocs). Keying by id changes the BTreeMap's traversal order vs the old
    // string keys, but `convert` re-sorts every node's children by
    // (retained desc, name asc), so the emitted tree is byte-identical.
    struct Builder {
        top_dominator_count: u64,
        shallow_heap: u64,
        retained_heap: u64,
        children: std::collections::BTreeMap<u32, Builder>,
    }
    impl Builder {
        fn new() -> Builder {
            Builder {
                top_dominator_count: 0,
                shallow_heap: 0,
                retained_heap: 0,
                children: std::collections::BTreeMap::new(),
            }
        }
    }

    // Segment interner: id -> owned name (for `convert`), and borrowed
    // segment slice -> id. The segment slices borrow from `g.class_names`
    // (never mutated in this function), so they are valid for the whole loop
    // and can key the map directly with no owned copy or unsafe.
    let mut seg_names: Vec<String> = Vec::new();
    let mut seg_ids: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();

    let mut root = Builder::new();
    // Reused across dominators so only its capacity persists (no per-dominator
    // Vec alloc); segments borrow from `raw_name` within each iteration.
    let mut segs: Vec<&str> = Vec::with_capacity(8);
    // Per-class memo of the resolved interned-segment-id PATH. The name that
    // drives the package path is `g.class_names[name_ci]`, and #classes (memo
    // size) is tiny vs #dominators (~329M on the 34GB dump), so the vast
    // majority of dominators repeat a class already seen. Caching the id path
    // per class turns ~329M `package_segments` name-parses + per-segment
    // `seg_ids` string-hash lookups into ~#classes of them, plus a cheap
    // `Vec<u32>` replay per dominator. `u32::MAX` marks "not yet computed".
    // Byte-exact: same interned ids, same tree, same `convert` output.
    let mut class_seg_path: Vec<Option<Vec<u32>>> = vec![None; class_count];
    for &i in &top_level {
        let idx = i as usize;
        // Use the class the object represents (for class objects), else own class.
        // Resolve class_obj_repr ONCE (it is a HashMap probe) rather than twice.
        let repr = class_obj_repr(g, idx);
        let name_ci: usize = if repr != undef && (repr as usize) < class_count {
            repr as usize
        } else {
            let ci = g.class_idx[idx] as usize;
            if ci < class_count {
                ci
            } else {
                continue;
            }
        };
        let retained = g.retained[idx];
        let shallow = g.shallow[idx] as u64;

        // Resolve (and memoize) the interned segment-id path for this class.
        // On a cache miss, parse the name into borrowed segments and intern each
        // (allocating a seg_name only on a segment's first global sighting), then
        // store the id path so future dominators of this class skip the parse and
        // the per-segment string hashing entirely.
        if class_seg_path[name_ci].is_none() {
            let raw_name: &str = &g.class_names[name_ci];
            package_segments(raw_name, &mut segs);
            let mut path: Vec<u32> = Vec::with_capacity(segs.len());
            for &seg in &segs {
                let id = match seg_ids.get(seg) {
                    Some(&id) => id,
                    None => {
                        let id = seg_names.len() as u32;
                        seg_names.push(seg.to_string());
                        seg_ids.insert(seg, id);
                        id
                    }
                };
                path.push(id);
            }
            class_seg_path[name_ci] = Some(path);
        }
        let path = class_seg_path[name_ci].as_ref().unwrap();

        // Accumulate at the root and at every node along the package path.
        root.top_dominator_count += 1;
        root.shallow_heap += shallow;
        root.retained_heap += retained;
        let mut node = &mut root;
        for &id in path {
            node = node.children.entry(id).or_insert_with(Builder::new);
            node.top_dominator_count += 1;
            node.shallow_heap += shallow;
            node.retained_heap += retained;
        }
    }
    t_tc!("package_tree_build");

    // Prune below-threshold nodes (top-down) and convert to the sorted model.
    // Children are keyed by interned segment id; `seg_names` maps id -> name.
    let total = root.retained_heap;
    let threshold_bp = PACKAGE_THRESHOLD_BP;
    fn convert(
        name: String,
        b: Builder,
        total: u64,
        threshold_bp: u32,
        seg_names: &[String],
    ) -> PackageNode {
        let mut children: Vec<PackageNode> = b
            .children
            .into_iter()
            // Prune any child below the threshold share of the total.
            .filter(|(_, cb)| {
                cb.retained_heap as u128 * 10_000 >= total as u128 * threshold_bp as u128
            })
            .map(|(id, cb)| {
                convert(
                    seg_names[id as usize].clone(),
                    cb,
                    total,
                    threshold_bp,
                    seg_names,
                )
            })
            .collect();
        // Sort retained-desc, tie-broken by name-asc.
        children.sort_by(|a, b| {
            b.retained_heap
                .cmp(&a.retained_heap)
                .then_with(|| a.name.cmp(&b.name))
        });
        PackageNode {
            name,
            top_dominator_count: b.top_dominator_count,
            shallow_heap: b.shallow_heap,
            retained_heap: b.retained_heap,
            children,
        }
    }
    let biggest_packages = convert(String::new(), root, total, threshold_bp, &seg_names);
    t_tc!("package_tree_convert");

    // Sort top_level by retained desc, tie-broken by index asc. The natural
    // comparator does two random-access loads into the ~4 GB `g.retained` array
    // per comparison (~O(n log n) of them) — cache-hostile and the dominant cost
    // of this phase (task #29 attribution). When the values fit, replace it with
    // a single-key sort that reads each `g.retained[idx]` EXACTLY ONCE into a
    // packed `u64` key, then sorts the cache-local key array (no indirection):
    //
    //   key = ((max_retained - retained) << idx_bits) | idx
    //
    // Sorting keys ASCENDING orders by `(max_retained - retained)` asc = retained
    // DESC, with `idx` in the low bits breaking ties ASC — byte-identical to the
    // old `retained desc, idx asc` comparator. This needs `idx` and
    // `max_retained` to co-fit in 64 bits; when they don't we fall back to the
    // exact comparator, so output is identical either way. The packed-key buffer
    // is one `Vec<u64>` (8 B/elem) — half the 16 B of a `(u64,u32)` decorate
    // (task #30, which blew the RSS ceiling at ~5.3 GB) and paying the random
    // `g.retained` loads once (unlike the radix retry, task #31, which re-read
    // them per pass for no win).
    // Precomputed size_distribution from the packed-u64 sort path.
    // Populated inside the fast branch below (where retained values are decoded
    // from keys, avoiding a second round of random g.retained reads after the
    // sort). The slow fallback branch leaves this None and the distribution block
    // below recomputes it from top_level + g.retained as before.
    let mut precomputed_distribution: Option<TopSizeDistribution> = None;
    if !top_level.is_empty() {
        // top_level is built index-ascending, so the last element is the largest
        // index; +1 gives the count of representable ids.
        let max_idx = *top_level.last().unwrap();
        let idx_bits = (u32::BITS - max_idx.leading_zeros()).max(1);
        let retained_bits = 64u32.saturating_sub(idx_bits);
        // Max retained over ALL objects (sequential scan, cache-friendly ~0.3s);
        // this is >= the max over top_level, so it is a conservative width bound.
        let max_retained = g.retained.iter().copied().max().unwrap_or(0);
        let fits = retained_bits >= 64 || max_retained < (1u64 << retained_bits);
        if fits && idx_bits < 64 {
            let idx_mask = (1u64 << idx_bits) - 1;
            let mut keys: Vec<u64> = top_level
                .iter()
                .map(|&i| {
                    let inv = max_retained - g.retained[i as usize];
                    (inv << idx_bits) | (i as u64 & idx_mask)
                })
                .collect();
            keys.sort_unstable();
            for (slot, &k) in top_level.iter_mut().zip(keys.iter()) {
                *slot = (k & idx_mask) as u32;
            }
            // Build size_distribution NOW from the sorted keys, decoding each
            // retained value as (max_retained - key >> idx_bits). This avoids a
            // second pass of 329M random g.retained reads after the sort.
            // Keys are sorted ascending ≡ retained descending, so:
            //   keys[0]   → largest retained (max)
            //   keys[k/2] → median retained
            //   keys[k-1] → smallest retained (min)
            let k = keys.len();
            let retained_of = |key: u64| max_retained - (key >> idx_bits);
            let max_r = retained_of(keys[0]);
            let min_r = retained_of(keys[k - 1]);
            let median_r = retained_of(keys[k / 2]);
            let mut total_ret: u64 = 0;
            let mut map: std::collections::BTreeMap<u64, u64> = std::collections::BTreeMap::new();
            for &key in &keys {
                let r = retained_of(key);
                total_ret = total_ret.saturating_add(r);
                let upper = if r <= 1 {
                    1
                } else {
                    r.checked_next_power_of_two().unwrap_or(u64::MAX)
                };
                *map.entry(upper).or_insert(0) += 1;
            }
            let buckets = map
                .into_iter()
                .map(|(upper_bytes, count)| SizeBucket { upper_bytes, count })
                .collect();
            precomputed_distribution = Some(TopSizeDistribution {
                buckets,
                count: k as u64,
                min: min_r,
                max: max_r,
                median: median_r,
                total: total_ret,
            });
            drop(keys);
        } else {
            top_level.sort_unstable_by(|&a, &b| {
                g.retained[b as usize]
                    .cmp(&g.retained[a as usize])
                    .then(a.cmp(&b))
            });
        }
    }
    t_tc!("sort_top_level");
    let size_distribution = if let Some(pd) = precomputed_distribution {
        pd
    } else {
        let k = top_level.len();
        if k == 0 {
            TopSizeDistribution::default()
        } else {
            let max = g.retained[top_level[0] as usize];
            let min = g.retained[top_level[k - 1] as usize];
            let median = g.retained[top_level[k / 2] as usize];
            let mut total_ret: u64 = 0;
            let mut map: std::collections::BTreeMap<u64, u64> = std::collections::BTreeMap::new();
            for &i in &top_level {
                let r = g.retained[i as usize];
                total_ret = total_ret.saturating_add(r);
                let upper = if r <= 1 {
                    1
                } else {
                    r.checked_next_power_of_two().unwrap_or(u64::MAX)
                };
                *map.entry(upper).or_insert(0) += 1;
            }
            let buckets = map
                .into_iter()
                .map(|(upper_bytes, count)| SizeBucket { upper_bytes, count })
                .collect();
            TopSizeDistribution {
                buckets,
                count: k as u64,
                min,
                max,
                median,
                total: total_ret,
            }
        }
    };

    t_tc!("size_distribution");

    // Biggest Objects
    // Owner attribution (Class#field) for the biggest objects, resolved only
    // under `--collections` (fields_by_size_raw is None otherwise). Build a
    // reverse map pointee-dense-idx -> Class#field, restricted to the top-N
    // object indices we're about to emit so it stays small. First writer wins
    // (raw groups are already deterministically ordered).
    let biggest_owner: std::collections::HashMap<u32, String> = {
        use std::collections::{HashMap, HashSet};
        match g.fields_by_size_raw.as_ref() {
            Some(raw) => {
                let targets: HashSet<u32> = top_level.iter().take(top_n).copied().collect();
                let mut m: HashMap<u32, String> = HashMap::new();
                for fs in raw {
                    for &p in &fs.pointee_indices {
                        if targets.contains(&p) {
                            m.entry(p)
                                .or_insert_with(|| format!("{}#{}", fs.holder_class, fs.field));
                        }
                    }
                }
                m
            }
            None => std::collections::HashMap::new(),
        }
    };
    let biggest_objects: Vec<ObjRow> = top_level
        .iter()
        .take(top_n)
        .map(|&i| {
            let idx = i as usize;
            let ci = g.class_idx[idx] as usize;
            // For class objects, show the class they represent (MAT parity: no
            // "class " prefix)
            let display_class = if class_obj_repr(g, idx) != undef {
                let repr = class_obj_repr(g, idx) as usize;
                if repr < g.class_names.len() {
                    pretty_class_name(&g.class_names[repr])
                } else if ci < g.class_names.len() {
                    pretty_class_name(&g.class_names[ci])
                } else {
                    String::from("?")
                }
            } else if ci < g.class_names.len() {
                pretty_class_name(&g.class_names[ci])
            } else {
                String::from("?")
            };

            let pct = if total_shallow > 0 {
                g.retained[idx] as f64 / total_shallow as f64 * 100.0
            } else {
                0.0
            };
            // Integer basis points of the retained share, for deterministic
            // JSON output (round-half-to-even via f64::round on *10000).
            let pct_bp = if total_shallow > 0 {
                (g.retained[idx] as f64 / total_shallow as f64 * 10000.0).round() as u64
            } else {
                0
            };

            let owner = biggest_owner.get(&i).cloned();
            // held_via: use stack frame annotation only when no field owner found.
            let held_via = if owner.is_none() {
                stack_held_via.get(&i).cloned()
            } else {
                None
            };
            ObjRow {
                obj_index_1based: idx + 1,
                display_class,
                shallow: g.shallow[idx] as u64,
                retained: g.retained[idx],
                pct_bp,
                pct,
                owner,
                held_via,
            }
        })
        .collect();

    t_tc!("biggest_objects");

    TopConsumers {
        biggest_objects,
        biggest_classes,
        threshold_bp,
        biggest_packages,
        size_distribution,
    }
}

#[cfg(test)]
mod fragmentation_tests {
    use super::*;

    #[test]
    fn fragmentation_ratio_zero_when_no_unreachable() {
        assert_eq!(compute_fragmentation_ratio(1000, 0), 0.0_f64);
    }

    #[test]
    fn fragmentation_ratio_half() {
        assert!((compute_fragmentation_ratio(500, 500) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn fragmentation_ratio_zero_empty_heap() {
        assert_eq!(compute_fragmentation_ratio(0, 0), 0.0_f64);
    }
}

#[cfg(test)]
mod top_level_sort_tests {
    // Proves the packed-u64 single-key sort used in `build_top_consumers`
    // (`sort_top_level`) produces byte-identical order to the reference
    // `retained desc, idx asc` comparator. The packing arithmetic
    // `((max_retained - retained) << idx_bits) | idx` is the delicate part, so
    // we exercise edge cases plus randomized inputs with heavy duplicate keys.

    // The reference: sort indices by retained desc, tie-broken by index asc.
    fn comparator_order(top_level: &[u32], retained: &[u64]) -> Vec<u32> {
        let mut v = top_level.to_vec();
        v.sort_unstable_by(|&a, &b| {
            retained[b as usize]
                .cmp(&retained[a as usize])
                .then(a.cmp(&b))
        });
        v
    }

    // Mirror of the production packed-key path (must stay in sync with
    // build_top_consumers). Returns None if the values don't co-fit in 64 bits,
    // in which case production falls back to the exact comparator.
    fn packed_order(top_level: &[u32], retained: &[u64]) -> Option<Vec<u32>> {
        if top_level.is_empty() {
            return Some(Vec::new());
        }
        // top_level is index-ascending in production; the test inputs honor that,
        // so the last element is the max index.
        let max_idx = *top_level.last().unwrap();
        let idx_bits = (u32::BITS - max_idx.leading_zeros()).max(1);
        let retained_bits = 64u32.saturating_sub(idx_bits);
        let max_retained = retained.iter().copied().max().unwrap_or(0);
        let fits = retained_bits >= 64 || max_retained < (1u64 << retained_bits);
        if !(fits && idx_bits < 64) {
            return None;
        }
        let idx_mask = (1u64 << idx_bits) - 1;
        let mut keys: Vec<u64> = top_level
            .iter()
            .map(|&i| {
                let inv = max_retained - retained[i as usize];
                (inv << idx_bits) | (i as u64 & idx_mask)
            })
            .collect();
        keys.sort_unstable();
        Some(keys.iter().map(|&k| (k & idx_mask) as u32).collect())
    }

    fn assert_matches(top_level: &[u32], retained: &[u64]) {
        let want = comparator_order(top_level, retained);
        let got = packed_order(top_level, retained)
            .expect("test inputs are constructed to fit in 64 bits");
        assert_eq!(got, want, "packed order != comparator order");
    }

    #[test]
    fn empty() {
        assert_matches(&[], &[]);
    }

    #[test]
    fn single() {
        assert_matches(&[0], &[42]);
    }

    #[test]
    fn distinct_retained() {
        // idx 0..5, retained increasing → sorted order is 4,3,2,1,0.
        let retained = vec![10u64, 20, 30, 40, 50];
        assert_matches(&[0, 1, 2, 3, 4], &retained);
    }

    #[test]
    fn all_equal_retained_preserves_idx_asc() {
        // Every retained equal → order is purely idx ascending (stable tiebreak).
        let retained = vec![7u64; 8];
        assert_matches(&[0, 1, 2, 3, 4, 5, 6, 7], &retained);
    }

    #[test]
    fn some_zero_retained() {
        let retained = vec![0u64, 5, 0, 5, 0];
        assert_matches(&[0, 1, 2, 3, 4], &retained);
    }

    #[test]
    fn max_retained_is_zero() {
        // max_retained == 0 → inv == 0 for all, key == idx, order is idx asc.
        let retained = vec![0u64, 0, 0];
        assert_matches(&[0, 1, 2], &retained);
    }

    #[test]
    fn randomized_heavy_duplicates() {
        // Deterministic xorshift PRNG; retained drawn from a tiny domain so many
        // ties collide, stressing the idx-asc tiebreak inside the packing.
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for trial in 0..200 {
            let n = (next() % 300) as usize + 1;
            // Small retained domain (0..=6) => heavy duplicate keys.
            let retained: Vec<u64> = (0..n).map(|_| next() % 7).collect();
            // top_level is index-ascending, a subset of 0..n. Include all for
            // simplicity (still index-ascending).
            let top_level: Vec<u32> = (0..n as u32).collect();
            let want = comparator_order(&top_level, &retained);
            let got = packed_order(&top_level, &retained)
                .unwrap_or_else(|| panic!("trial {trial}: unexpected non-fit"));
            assert_eq!(got, want, "trial {trial}: n={n}");
        }
    }

    #[test]
    fn large_retained_values() {
        // Retained values in the hundreds of millions with a small index space:
        // idx_bits is tiny, retained_bits huge, so packing must not overflow.
        let retained = vec![900_000_000u64, 100, 500_000_000, 900_000_000, 0];
        assert_matches(&[0, 1, 2, 3, 4], &retained);
    }
}

#[cfg(test)]
mod attribution_tests {
    use super::*;

    fn rec(
        container_idx: u32,
        holder: &str,
        field: &str,
        kind: u8,
        container_class: &str,
        elements: u64,
    ) -> AttributionRaw {
        AttributionRaw {
            container_idx,
            holder_class: holder.to_string(),
            field: field.to_string(),
            container_kind: kind,
            container_class: container_class.to_string(),
            elements,
            // Arrays carry real capacity; here default it to elements so the
            // biggest-single capacity mirrors elements in these tests.
            capacity: elements,
        }
    }

    /// Empty holder-instance map ⇒ every row's `holder_instances` is 0. Used by
    /// most aggregate tests that don't exercise Metric A.
    fn no_holders() -> std::collections::HashMap<String, u64> {
        std::collections::HashMap::new()
    }

    /// Two keys with different total_elements come out in DESC order; the pure
    /// helper's most_overall/biggest_single are populated as expected.
    #[test]
    fn test_ordering_desc_by_elements() {
        // key A: com/foo/Big#items, one container idx 0 with 100 elements.
        // key B: com/foo/Small#items, one container idx 1 with 10 elements.
        let raw = vec![
            rec(0, "com/foo/Big", "items", 0, "java/util/ArrayList", 100),
            rec(1, "com/foo/Small", "items", 0, "java/util/ArrayList", 10),
        ];
        let retained = vec![5000u64, 500u64];
        // Metric A: holder-instance lookup keyed by PRETTIFIED class name.
        let mut holders = std::collections::HashMap::new();
        holders.insert("com.foo.Big".to_string(), 3u64);
        let ca = aggregate_collection_attribution(&raw, &retained, false, &holders, 8);
        assert_eq!(ca.most_overall.len(), 2);
        assert_eq!(ca.most_overall[0].holder_class, "com/foo/Big");
        assert_eq!(ca.most_overall[0].total_elements, 100);
        assert_eq!(ca.most_overall[0].total_retained, 5000);
        assert_eq!(
            ca.most_overall[0].holder_instances, 3,
            "holder_instances populated from the map"
        );
        assert_eq!(ca.most_overall[1].holder_instances, 0, "absent holder ⇒ 0");
        assert_eq!(ca.most_overall[1].holder_class, "com/foo/Small");
        // biggest_single mirrors the ordering.
        assert_eq!(ca.biggest_single[0].holder_class, "com/foo/Big");
        assert_eq!(ca.biggest_single[0].elements, 100);
        assert_eq!(
            ca.biggest_single[0].capacity, 100,
            "rec() defaults capacity to elements"
        );
        assert_eq!(ca.biggest_single[0].container_class, "java.util.ArrayList");
        assert!(!ca.truncated);
    }

    /// Distinct-container dedup: two records with the SAME container_idx under
    /// one key count that container's elements/retained ONCE, container_count 1.
    #[test]
    fn test_distinct_container_dedup() {
        // Two Cache instances share ONE map (container idx 0): the join emits
        // two rows with the same container_idx.
        let raw = vec![
            rec(0, "com/foo/Cache", "map", 0, "java/util/HashMap", 42),
            rec(0, "com/foo/Cache", "map", 0, "java/util/HashMap", 42),
        ];
        let retained = vec![9000u64];
        let ca = aggregate_collection_attribution(&raw, &retained, false, &no_holders(), 8);
        assert_eq!(ca.most_overall.len(), 1);
        let row = &ca.most_overall[0];
        assert_eq!(row.container_count, 1, "shared container counted once");
        assert_eq!(row.total_elements, 42, "elements not double-counted");
        assert_eq!(row.total_retained, 9000, "retained not double-counted");
    }

    /// Mixed kind: two DISTINCT containers of different kinds under one key
    /// yield container_kind == "mixed".
    #[test]
    fn test_mixed_kind() {
        // com/foo/Holder#data points at a collection (idx 0) and an object
        // array (idx 1) — distinct containers, different kinds.
        let raw = vec![
            rec(0, "com/foo/Holder", "data", 0, "java/util/ArrayList", 5),
            rec(1, "com/foo/Holder", "data", 6, "[Ljava/lang/Object;", 7),
        ];
        let retained = vec![100u64, 200u64];
        let ca = aggregate_collection_attribution(&raw, &retained, false, &no_holders(), 8);
        assert_eq!(ca.most_overall.len(), 1);
        assert_eq!(ca.most_overall[0].container_kind, "mixed");
        assert_eq!(ca.most_overall[0].container_count, 2);
        assert_eq!(ca.most_overall[0].total_elements, 12);
        assert_eq!(ca.most_overall[0].total_retained, 300);
    }

    /// Single-kind key keeps its own label (regression: not "mixed").
    #[test]
    fn test_single_kind_label() {
        let raw = vec![rec(0, "com/foo/H", "arr", 7, "[I", 3)];
        let retained = vec![64u64];
        let ca = aggregate_collection_attribution(&raw, &retained, true, &no_holders(), 8);
        assert_eq!(ca.most_overall[0].container_kind, "primitive array");
        assert!(ca.truncated);
    }

    /// Retained lookup is defensive: an out-of-range container_idx contributes 0.
    #[test]
    fn test_out_of_range_retained_is_zero() {
        let raw = vec![rec(99, "com/foo/H", "f", 0, "java/util/ArrayList", 1)];
        let retained = vec![10u64]; // idx 99 is out of range
        let ca = aggregate_collection_attribution(&raw, &retained, false, &no_holders(), 8);
        assert_eq!(ca.most_overall[0].total_retained, 0);
        assert_eq!(ca.biggest_single[0].retained, 0);
    }
}

#[cfg(test)]
mod dom_subtree_tests {
    use super::*;

    #[test]
    fn obj_graph_flat_node_has_dom_subtree_count() {
        let n = ObjGraphFlatNode {
            display_class: "Foo".to_string(),
            shallow: 0,
            retained: 0,
            edges_unknown: false,
            edges_truncated: false,
            idom: None,
            dom_subtree_count: 1,
            subtree_classes: Vec::new(),
        };
        assert_eq!(n.dom_subtree_count, 1);
    }
}

// ── Field statistics ─────────────────────────────────────────────────────────

/// Compute per-class reference-field statistics for the top-50 most common
/// classes by instance count. For each class, counts total outbound reference
/// edges (non-null refs in the CSR forward graph) and sums the retained sizes
/// of their targets. Field names are empty strings because the CSR does not
/// carry field names unless `--ref-paths` was used.
pub fn build_field_stats(g: &Graph) -> FieldStats {
    // Count instances per class histogram row index
    let mut row_counts: Vec<u64> = vec![0u64; g.class_names.len()];
    for &ci in &g.class_idx {
        if (ci as usize) < row_counts.len() {
            row_counts[ci as usize] += 1;
        }
    }

    // Top-50 classes by instance count
    let mut ranked: Vec<(usize, u64)> = row_counts
        .iter()
        .enumerate()
        .filter(|(_, c)| **c > 0)
        .map(|(i, &c)| (i, c))
        .collect();
    ranked.sort_unstable_by_key(|r: &(usize, u64)| std::cmp::Reverse(r.1));
    ranked.truncate(50);

    let target_rows: std::collections::HashSet<usize> = ranked.iter().map(|(i, _)| *i).collect();
    let retained_len = g.retained.len();

    // Per-class, per-field accumulators: (non_null_counts, retained_sums)
    use std::collections::HashMap;
    let mut acc_map: HashMap<usize, (Vec<u64>, Vec<u64>)> = HashMap::new();
    for &(ci, _) in &ranked {
        let n_fields = g
            .class_ref_field_names
            .get(ci)
            .map(|v| v.len())
            .unwrap_or(0);
        acc_map.insert(ci, (vec![0u64; n_fields], vec![0u64; n_fields]));
    }

    for (obj, &ci_u32) in g.class_idx.iter().enumerate() {
        let ci = ci_u32 as usize;
        if !target_rows.contains(&ci) {
            continue;
        }
        let (nn, rt) = match acc_map.get_mut(&ci) {
            Some(v) => v,
            None => continue,
        };
        if obj + 1 >= g.fwd_offsets.len() {
            continue;
        }
        let start = g.fwd_offsets[obj] as usize;
        let end = g.fwd_offsets[obj + 1] as usize;
        // Skip pos start+0 (class-object edge); field slots start at start+1
        for pos in (start + 1)..end {
            let slot = pos - (start + 1);
            if slot >= nn.len() {
                break;
            }
            // g.fwd_targets is ChunkU32; use .get(pos) to index
            let tgt = g.fwd_targets.get(pos) as usize;
            if tgt < retained_len {
                nn[slot] += 1;
                rt[slot] += g.retained[tgt];
            }
        }
    }

    let classes = ranked
        .into_iter()
        .map(|(ci, instance_count)| {
            let class_name = g.class_names[ci].clone();
            let names = g.class_ref_field_names.get(ci).cloned().unwrap_or_default();
            let (nn, rt) = acc_map.remove(&ci).unwrap_or_default();

            let ref_fields = if names.is_empty() {
                // No reference field schema for this class — report no field data
                vec![FieldRefStat {
                    field_name: String::new(),
                    null_count: 0,
                    non_null_count: 0,
                    total_retained: 0,
                }]
            } else {
                names
                    .iter()
                    .enumerate()
                    .map(|(slot, name)| {
                        let nonnull = nn.get(slot).copied().unwrap_or(0);
                        FieldRefStat {
                            field_name: name.clone(),
                            null_count: instance_count.saturating_sub(nonnull),
                            non_null_count: nonnull,
                            total_retained: rt.get(slot).copied().unwrap_or(0),
                        }
                    })
                    .collect()
            };
            ClassFieldStats {
                class_name,
                instance_count,
                ref_fields,
            }
        })
        .collect();

    FieldStats { classes }
}

#[cfg(test)]
mod leak_indicator_tests {
    use super::*;

    #[test]
    fn anonymous_class_patterns() {
        // These should match:
        assert!(is_anonymous_class("com/example/Foo$1")); // anon inner
        assert!(is_anonymous_class("com/example/Foo$$Lambda$42/0x1234")); // lambda
        assert!(is_anonymous_class("com/example/Foo$Proxy1")); // proxy
        assert!(is_anonymous_class("com/example/$$Anon")); // anon
        // These should NOT match:
        assert!(!is_anonymous_class("com/example/Foo$Bar")); // named inner
        assert!(!is_anonymous_class("java/lang/String")); // plain class
    }
}
