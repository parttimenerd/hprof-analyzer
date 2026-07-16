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
/// Cap on the TOTAL number of nodes emitted across a group suspect's merged
/// shortest-paths-to-GC-roots prefix tree. Once reached, existing matching
/// nodes keep accumulating counts/retained (so totals stay meaningful) but no
/// new branches are created — deterministic, RSS-bounded.
const MERGED_PATH_MAX_NODES: usize = 60;

// ── Model construction ───────────────────────────────────────────────────────

/// Compute all report aggregates from the graph.
///
/// Ordering mirrors the previous three separate render calls so callers keep
/// the same free-as-you-go RSS discipline: the system-overview group is
/// computed first (the only reader of `has_same_class_ancestor`), then the
/// leak-suspect group (the only reader of `dc_offsets`/`dc_targets`), then top
/// consumers. Because the returned `Report` holds only small aggregates, the
/// caller may free `has_same_class_ancestor` and `dc_offsets`/`dc_targets`
/// immediately after this returns.
pub fn build_model(
    g: &Graph,
    dc_offsets: &[u32],
    dc_targets: &[u32],
    leak_children_cap: usize,
    depth_counts: &[u64],
    opts: &crate::AnalyzeOptions,
    alloc_sites: Option<AllocSites>,
) -> Report {
    let generated = now_iso8601();
    crate::trace::probe("build_model: before system_overview aggregates");
    let overview = build_system_overview(g, depth_counts, opts.top_consumers);
    crate::trace::probe("build_model: after system_overview aggregates");
    let leaks = build_leak_suspects(
        g,
        dc_offsets,
        dc_targets,
        leak_children_cap,
        opts.root_path_max_depth,
        opts.dominator_tree_max_nodes,
        opts.dominator_tree_max_depth,
    );
    crate::trace::probe("build_model: after leak_suspects aggregates");
    let top = build_top_consumers(g, opts.top_consumers);
    crate::trace::probe("build_model: after top_consumers aggregates");
    let threads = build_thread_overview(g);
    crate::trace::probe("build_model: after thread_overview aggregates");
    let top_components = build_top_components(&overview);
    crate::trace::probe("build_model: after top_components aggregates");
    let dominator_analysis = build_dominator_analysis(g, dc_offsets, dc_targets);
    crate::trace::probe("build_model: after dominator_analysis aggregates");
    let references = build_references(g);
    crate::trace::probe("build_model: after references only-weakly-retained rollup");
    Report {
        schema_version: SCHEMA_VERSION,
        generated,
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
        collection_attribution: build_collection_attribution(g),
        leak_indicators: build_leak_indicators(g),
    }
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

fn build_leak_indicators(g: &Graph) -> LeakIndicators {
    // 1. Anonymous/generated class count — one entry per distinct class in class_names.
    let anonymous_class_count = g.class_names.iter()
        .filter(|n| is_anonymous_class(n))
        .count() as u64;

    // 2. ThreadLocalMap$Entry null-key count.
    // A cleared referent means the entry has no forward edges to a live non-zero object.
    // Excluded (weak-ref) edges are high-bit tagged (0x8000_0000); we mask to get the index.
    let tl_suffix = "ThreadLocal$ThreadLocalMap$Entry";
    let n_nodes = g.fwd_offsets.len().saturating_sub(1);
    let mut thread_local_null_key_count: u64 = 0;
    for i in 0..n_nodes {
        let ci = g.class_idx[i] as usize;
        if ci >= g.class_names.len() { continue; }
        if !g.class_names[ci].ends_with(tl_suffix) { continue; }
        let start = g.fwd_offsets[i] as usize;
        let end = g.fwd_offsets[i + 1] as usize;
        let has_live_referent = g.fwd_targets[start..end]
            .iter()
            .any(|&t| (t & 0x7FFF_FFFF) != 0);
        if !has_live_referent {
            thread_local_null_key_count += 1;
        }
    }

    // 3. DirectByteBuffer capacity sum — already computed in pass2.
    let direct_byte_buffer_capacity_sum = g.direct_byte_buffer_capacity_sum;

    LeakIndicators {
        anonymous_class_count,
        thread_local_null_key_count,
        direct_byte_buffer_capacity_sum,
    }
}

/// Pretty class-display name for object index `i`, matching the derivation used
/// throughout the report (`build_dominator_analysis`'s `display_of`): resolve
/// the object's class row and render it via `pretty_class_name`. Returns an
/// empty string when the class row is out of range.
fn class_display(g: &Graph, i: usize) -> String {
    let ci = g.class_idx[i] as usize;
    if ci < g.class_names.len() {
        pretty_class_name(&g.class_names[ci])
    } else {
        String::new()
    }
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
        let mut by_class: HashMap<String, (u64, u64)> = HashMap::new();
        for &ri in &g.reference_referent_idx[kind] {
            let i = ri as usize;
            if g.idom[i] != undef {
                continue; // has a strong dominator -> not only-weakly-retained
            }
            let e = by_class.entry(class_display(g, i)).or_insert((0, 0));
            e.0 += 1;
            e.1 += g.shallow[i] as u64;
        }
        let mut rows: Vec<RefStatClassRow> = by_class
            .into_iter()
            .map(|(pretty_class, (objects, shallow))| RefStatClassRow {
                pretty_class,
                objects,
                shallow,
            })
            .collect();
        // Deterministic: objects desc, then pretty_class asc.
        rows.sort_unstable_by(|a, b| {
            b.objects
                .cmp(&a.objects)
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
        0 => "collection",
        1 => "object array",
        2 => "primitive array",
        _ => "mixed",
    }
}

/// Build the container-attribution rankings from the raw field-decode records,
/// attaching each container's retained size via its dense index. `None` when
/// `--collections` was off (the raw vec is absent). Aggregates two rankings:
/// most_overall (per Class#field, total elements/retained across all its
/// containers, distinct-container count) and biggest_single (per Class#field,
/// the single largest container by element count).
fn build_collection_attribution(g: &Graph) -> Option<CollectionAttribution> {
    let raw = g.collection_attribution_raw.as_ref()?;
    Some(aggregate_collection_attribution(
        raw,
        &g.retained,
        g.collection_attribution_truncated,
    ))
}

/// Pure aggregation core (no `Graph` dependency, so it is directly unit
/// testable): fold the raw attribution records into the two `Class#field`
/// rankings, looking up each container's retained size in `retained` by its
/// dense object index. See [`build_collection_attribution`] for the semantics.
fn aggregate_collection_attribution(
    raw: &[AttributionRaw],
    retained: &[u64],
    truncated: bool,
) -> CollectionAttribution {
    use std::collections::HashMap;

    // most_overall accumulator, keyed by (holder_class, field).
    struct OverallAcc {
        total_elements: u64,
        total_retained: u64,
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
    }

    let mut overall: HashMap<(String, String), OverallAcc> = HashMap::new();
    let mut biggest: HashMap<(String, String), BiggestAcc> = HashMap::new();

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
            seen: std::collections::HashSet::new(),
            first_kind: rec.container_kind,
            mixed: false,
        });
        if acc.seen.insert(rec.container_idx) {
            acc.total_elements += rec.elements;
            acc.total_retained += retained_bytes;
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
        });
        if rec.elements > b.elements || (rec.elements == b.elements && retained_bytes > b.retained)
        {
            b.elements = rec.elements;
            b.retained = retained_bytes;
            b.container_class = rec.container_class.clone();
        }
    }

    let mut most_overall: Vec<FieldAttributionRow> = overall
        .into_iter()
        .map(|((holder_class, field), acc)| FieldAttributionRow {
            holder_class,
            field,
            container_kind: if acc.mixed {
                "mixed".to_string()
            } else {
                kind_label(acc.first_kind).to_string()
            },
            total_elements: acc.total_elements,
            total_retained: acc.total_retained,
            container_count: acc.seen.len() as u64,
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

    CollectionAttribution {
        most_overall,
        biggest_single,
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
    let remap = class_row_remap(g);
    let mut dom_count = vec![0u64; class_count]; // #dominator objects of this class
    let mut domd_count = vec![0u64; class_count]; // #objects immediately dominated
    let mut dom_shallow = vec![0u64; class_count];
    let mut domd_shallow = vec![0u64; class_count];
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
            domd_count[pci] += 1;
            domd_shallow[pci] += g.shallow[c as usize] as u64;
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
    let immediate_dominators = ImmediateDominators { rows };

    DominatorAnalysis {
        big_drops,
        immediate_dominators,
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
pub(crate) fn build_thread_overview(g: &Graph) -> ThreadOverview {
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
                                    .cloned()
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
            let (significant_frames, max_local_retained) = build_significant_frames(g, t, retained);

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
    thread_retained: u64,
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
                    .cloned()
                    .unwrap_or_else(|| "<unknown>".to_string());
                let retained = g.retained.get(li as usize).copied().unwrap_or(0);
                max_local_retained = max_local_retained.max(retained);
                let pct = if thread_retained > 0 {
                    retained as f64 / thread_retained as f64 * 100.0
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
    if denom == 0 { 0.0 } else { unreachable_shallow as f64 / denom as f64 }
}

/// Compute the retained heap share of the single largest class in integer basis
/// points (100 bp = 1%). The histogram must already be sorted by retained
/// descending (as produced by `build_system_overview`). Returns 0 when empty.
fn compute_top_class_concentration_bp(histogram: &[crate::report::HistRow], total_retained: u64) -> u32 {
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
fn build_system_overview(g: &Graph, depth_counts: &[u64], top_n: usize) -> SystemOverview {
    let n = g.n;
    let undef = u32::MAX;

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
    // Per-class tally of unreachable objects (idom == undef), bounded by #classes.
    let mut unreach_count: Vec<u64> = vec![0; class_count];
    let mut unreach_shallow: Vec<u64> = vec![0; class_count];
    for i in 0..n {
        if g.idom[i] != undef {
            total_objects += 1;
            total_shallow += g.shallow[i] as u64;
        } else {
            unreachable_count += 1;
            unreachable_shallow += g.shallow[i] as u64;
            let ci = g.class_idx[i] as usize;
            if ci < class_count {
                let ci = remap[ci] as usize;
                unreach_count[ci] += 1;
                unreach_shallow[ci] += g.shallow[i] as u64;
            }
        }
    }

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
    const KIND_ORDER: [&str; 4] = [
        "Instances",
        "Object arrays",
        "Primitive arrays",
        "Class objects",
    ];
    let heap_composition = {
        let mut objs = [0u64; 4];
        let mut sh = [0u64; 4];
        let idx = |k: &str| KIND_ORDER.iter().position(|&x| x == k).unwrap();
        for i in 0..n {
            if g.idom[i] == undef {
                continue;
            }
            let b = idx(object_kind(g, i));
            objs[b] += 1;
            sh[b] += g.shallow[i] as u64;
        }
        // Synthetic <system class loader> counts as an Instance, matching how
        // total_objects/total_shallow count it above.
        if let Some(sz) = g.system_classloader_shallow {
            let b = idx("Instances");
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
        HeapComposition { by_kind }
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
        let vroot = n as u32;
        let mut tops: Vec<u64> = (0..n)
            .filter(|&i| g.idom[i] == vroot)
            .map(|i| g.retained[i])
            .collect();
        tops.sort_unstable_by(|a, b| b.cmp(a)); // retained desc
        let denom = total_shallow.max(1);
        let bp = |sum: u64| -> u32 { ((sum as u128 * 10_000) / denom as u128) as u32 };
        let prefix = |k: usize| -> u64 { tops.iter().take(k).sum() };
        let total_retained: u64 = tops.iter().sum();
        let one_pct = denom / 100;
        let num_objects_ge_1pct = tops.iter().filter(|&&r| r >= one_pct).count() as u64;
        RetentionSummary {
            total_retained,
            top1_bp: bp(prefix(1)),
            top10_bp: bp(prefix(10)),
            top100_bp: bp(prefix(100)),
            num_objects_ge_1pct,
        }
    };
    // Count reachable class-dump objects (objects that ARE Java classes, with defined idom)
    let undef_u32 = u32::MAX;
    let classes_loaded = (0..n)
        .filter(|&i| class_obj_repr(g, i) != u32::MAX && g.idom[i] != undef_u32)
        .count() as u64;

    // Distinct class loaders among the reachable class objects counted above.
    // Each reachable class object maps to its histogram row via
    // class_obj_class_idx, and the row carries the loader address. Mirrors the
    // classes_loaded domain so the two scalars agree on "which classes".
    let classloaders_loaded = {
        let mut set: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for i in 0..n {
            if class_obj_repr(g, i) != u32::MAX && g.idom[i] != undef_u32 {
                let lid = g
                    .class_obj_class_idx
                    .get(&(i as u32))
                    .and_then(|&row| g.class_loader_id.get(row as usize).copied())
                    .unwrap_or(0);
                set.insert(lid);
            }
        }
        set.len() as u64
    };

    // Class histogram: per-class instance count, shallow total, retained total
    let mut inst_count: Vec<u64> = vec![0; class_count];
    let mut shallow_total: Vec<u64> = vec![0; class_count];
    let mut class_retained: Vec<u64> = vec![0; class_count];
    let mut max_shallow: Vec<u64> = vec![0; class_count];

    // First pass: for all reachable objects
    for i in 0..n {
        if g.idom[i] == undef {
            continue;
        }
        let ci = g.class_idx[i] as usize;
        if ci >= class_count {
            continue;
        }
        let ci = remap[ci] as usize;
        inst_count[ci] += 1;
        shallow_total[ci] += g.shallow[i] as u64;
        max_shallow[ci] = max_shallow[ci].max(g.shallow[i] as u64);
        // MAT top-ancestor semantics: only count retained of objects with no
        // same-class (or class-object) ancestor in the dominator tree.
        if !g.has_same_class_ancestor.get(i) {
            class_retained[ci] += g.retained[i];
        }
    }

    // Second pass: for each class object, add its retained to the class it represents
    for i in 0..n {
        if g.idom[i] == undef {
            continue;
        }
        let repr = class_obj_repr(g, i);
        if repr == undef {
            continue;
        }
        let ci = repr as usize;
        if ci >= class_count {
            continue;
        }
        let ci = remap[ci] as usize;
        class_retained[ci] += g.retained[i];
    }

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
    let histogram: Vec<HistRow> = order
        .into_iter()
        .map(|ci| HistRow {
            pretty_class: pretty_class_name(&g.class_names[ci]),
            instances: inst_count[ci],
            shallow: shallow_total[ci],
            retained: class_retained[ci],
            max_instance_shallow: max_shallow[ci],
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
        })
        .collect();

    // Per-class unreachable-objects histogram: capped, shallow-desc. Only
    // canonical rows (remap[ci] == ci) with unreachable objects are emitted.
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
        let mut by_type: HashMap<String, (u64, u64)> = HashMap::new();
        for (&idx, &ty) in g.gc_root_indices.iter().zip(g.gc_root_types.iter()) {
            if let Some(label) = gc_root_type_label_opt(ty) {
                let retained = g.retained.get(idx as usize).copied().unwrap_or(0);
                let e = by_type.entry(label.to_string()).or_insert((0, 0));
                e.0 += 1;
                e.1 = e.1.saturating_add(retained);
            }
        }
        let mut rows: Vec<crate::report::GcRootRetainedRow> = by_type
            .into_iter()
            .map(|(root_type, (count, retained))| crate::report::GcRootRetainedRow {
                root_type,
                count,
                retained,
            })
            .collect();
        rows.sort_by(|a, b| b.retained.cmp(&a.retained).then(a.root_type.cmp(&b.root_type)));
        rows
    };

    // Compressed OOPs: references narrower than identifiers (id_size 8 -> ref 4).
    let compressed_oops = Some(g.ref_size < g.id_size);
    let dump_creation = if g.header_timestamp_ms != 0 {
        Some(g.header_timestamp_ms as i64)
    } else {
        None
    };

    SystemOverview {
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
        heap_fragmentation_ratio,
        top_class_concentration_bp,
        gc_roots_retained_by_type,
    }
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
            children: Vec::new(),
        });

        for &m in members {
            // Collect the ordered dominator-chain hops [m, idom[m], ...], stopping
            // at the terminal GC root (idom == vroot), an unreachable node
            // (idom == undef), or the depth cap. The terminal root IS included,
            // exactly mirroring the single-suspect `root_path` walk.
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
                // find-or-create a child of `node` with this label.
                let existing = arena[node]
                    .children
                    .iter()
                    .copied()
                    .find(|&c| arena[c].display_class == label);
                let child = match existing {
                    Some(c) => c,
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
                    let best_child = dom_children(cur)
                        .iter()
                        .max_by_key(|&&c| g.retained[c as usize]);
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
    // This mirrors MAT's Leak Suspects "path to the accumulation point", which is
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
                chain.push(RootPathStep {
                    obj_index_1based: cur + 1,
                    display_class: display_of(cur),
                    retained: g.retained[cur],
                    root_type_label,
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
        let upper = if r <= 1 { 1 } else { r.next_power_of_two() };
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

/// Build the "Top Consumers" model: biggest objects (top-level dominators by
/// retained), biggest classes, and the pruned package tree. Bounded reductions
/// over the graph; no per-object Vec is retained.
fn build_top_consumers(g: &Graph, top_n: usize) -> TopConsumers {
    let n = g.n;
    let vroot = n as u32;
    let undef = u32::MAX;
    let class_count = g.class_names.len();

    // Collect top-level dominators
    let mut top_level: Vec<u32> = Vec::new();
    for i in 0..n {
        if g.idom[i] == vroot {
            top_level.push(i as u32);
        }
    }

    // Total shallow of all reachable objects (MAT parity: pct base for Biggest Objects)
    let total_shallow: u64 = (0..n)
        .filter(|&i| g.idom[i] != undef)
        .map(|i| g.shallow[i] as u64)
        .sum();

    // Sort by retained desc for biggest objects, with tie-breaker on ascending
    // object index (top_level built in ascending order).
    let mut sorted_top: Vec<u32> = top_level.clone();
    sorted_top.sort_unstable_by(|&a, &b| {
        g.retained[b as usize]
            .cmp(&g.retained[a as usize])
            .then(a.cmp(&b))
    });

    // Retained-size distribution over EVERY top-level dominator (independent of
    // the top_n truncation used for `biggest_objects`).
    let sorted_retained: Vec<u64> = sorted_top.iter().map(|&i| g.retained[i as usize]).collect();
    let size_distribution = build_size_distribution(&sorted_retained);

    // Biggest Objects
    let biggest_objects: Vec<ObjRow> = sorted_top
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

            ObjRow {
                obj_index_1based: idx + 1,
                display_class,
                shallow: g.shallow[idx] as u64,
                retained: g.retained[idx],
                pct_bp,
                pct,
            }
        })
        .collect();

    // Biggest Classes by Retained Heap
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

    // Biggest Packages: build a pruned package TREE (MAT PackageTreeResult
    // parity). Accumulate cumulative retained/shallow/count into a BTreeMap-keyed
    // builder so the model has no HashMap, then convert + sort + prune.
    struct Builder {
        top_dominator_count: u64,
        shallow_heap: u64,
        retained_heap: u64,
        children: std::collections::BTreeMap<String, Builder>,
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

    let mut root = Builder::new();
    for &i in &top_level {
        let idx = i as usize;
        // Use the class the object represents (for class objects), else own class.
        let raw_name = if class_obj_repr(g, idx) != undef {
            let repr = class_obj_repr(g, idx) as usize;
            if repr < g.class_names.len() {
                &g.class_names[repr]
            } else {
                let ci = g.class_idx[idx] as usize;
                if ci < g.class_names.len() {
                    &g.class_names[ci]
                } else {
                    continue;
                }
            }
        } else {
            let ci = g.class_idx[idx] as usize;
            if ci < g.class_names.len() {
                &g.class_names[ci]
            } else {
                continue;
            }
        };
        let retained = g.retained[idx];
        let shallow = g.shallow[idx] as u64;
        let path = package_path(raw_name);

        // Accumulate at the root and at every node along the dotted path.
        root.top_dominator_count += 1;
        root.shallow_heap += shallow;
        root.retained_heap += retained;
        let mut node = &mut root;
        for seg in path.split('.') {
            node = node
                .children
                .entry(seg.to_string())
                .or_insert_with(Builder::new);
            node.top_dominator_count += 1;
            node.shallow_heap += shallow;
            node.retained_heap += retained;
        }
    }

    // Prune below-threshold nodes (top-down) and convert to the sorted model.
    let total = root.retained_heap;
    let threshold_bp = PACKAGE_THRESHOLD_BP;
    fn convert(name: String, b: Builder, total: u64, threshold_bp: u32) -> PackageNode {
        let mut children: Vec<PackageNode> = b
            .children
            .into_iter()
            // Prune any child below the threshold share of the total.
            .filter(|(_, cb)| {
                cb.retained_heap as u128 * 10_000 >= total as u128 * threshold_bp as u128
            })
            .map(|(seg, cb)| convert(seg, cb, total, threshold_bp))
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
    let biggest_packages = convert(String::new(), root, total, threshold_bp);

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
        }
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
        let ca = aggregate_collection_attribution(&raw, &retained, false);
        assert_eq!(ca.most_overall.len(), 2);
        assert_eq!(ca.most_overall[0].holder_class, "com/foo/Big");
        assert_eq!(ca.most_overall[0].total_elements, 100);
        assert_eq!(ca.most_overall[0].total_retained, 5000);
        assert_eq!(ca.most_overall[1].holder_class, "com/foo/Small");
        // biggest_single mirrors the ordering.
        assert_eq!(ca.biggest_single[0].holder_class, "com/foo/Big");
        assert_eq!(ca.biggest_single[0].elements, 100);
        assert_eq!(ca.biggest_single[0].container_class, "java/util/ArrayList");
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
        let ca = aggregate_collection_attribution(&raw, &retained, false);
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
            rec(1, "com/foo/Holder", "data", 1, "[Ljava/lang/Object;", 7),
        ];
        let retained = vec![100u64, 200u64];
        let ca = aggregate_collection_attribution(&raw, &retained, false);
        assert_eq!(ca.most_overall.len(), 1);
        assert_eq!(ca.most_overall[0].container_kind, "mixed");
        assert_eq!(ca.most_overall[0].container_count, 2);
        assert_eq!(ca.most_overall[0].total_elements, 12);
        assert_eq!(ca.most_overall[0].total_retained, 300);
    }

    /// Single-kind key keeps its own label (regression: not "mixed").
    #[test]
    fn test_single_kind_label() {
        let raw = vec![rec(0, "com/foo/H", "arr", 2, "[I", 3)];
        let retained = vec![64u64];
        let ca = aggregate_collection_attribution(&raw, &retained, true);
        assert_eq!(ca.most_overall[0].container_kind, "primitive array");
        assert!(ca.truncated);
    }

    /// Retained lookup is defensive: an out-of-range container_idx contributes 0.
    #[test]
    fn test_out_of_range_retained_is_zero() {
        let raw = vec![rec(99, "com/foo/H", "f", 0, "java/util/ArrayList", 1)];
        let retained = vec![10u64]; // idx 99 is out of range
        let ca = aggregate_collection_attribution(&raw, &retained, false);
        assert_eq!(ca.most_overall[0].total_retained, 0);
        assert_eq!(ca.biggest_single[0].retained, 0);
    }
}

#[cfg(test)]
mod leak_indicator_tests {
    use super::*;

    #[test]
    fn anonymous_class_patterns() {
        // These should match:
        assert!(is_anonymous_class("com/example/Foo$1"));           // anon inner
        assert!(is_anonymous_class("com/example/Foo$$Lambda$42/0x1234")); // lambda
        assert!(is_anonymous_class("com/example/Foo$Proxy1"));      // proxy
        assert!(is_anonymous_class("com/example/$$Anon"));          // anon
        // These should NOT match:
        assert!(!is_anonymous_class("com/example/Foo$Bar"));        // named inner
        assert!(!is_anonymous_class("java/lang/String"));           // plain class
    }
}
