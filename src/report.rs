//! Report generation: system overview, leak suspects, top consumers.
//!
//! Rendering goes through an explicit data model: `build_model` reads the
//! `Graph` (including the large per-object arrays) and computes only bounded
//! aggregates into a `Report`; `render_markdown` formats a `Report` into the
//! Markdown output. This keeps peak RSS bounded (the model never stores a
//! per-object Vec) and makes ordering deterministic.

use crate::pass2::Graph;

#[inline]
fn class_obj_repr(g: &Graph, i: usize) -> u32 {
    g.class_obj_class_idx
        .get(&(i as u32))
        .copied()
        .unwrap_or(u32::MAX)
}

/// True iff `name` is a JVM primitive-array class descriptor (`[B`, `[I`, …):
/// a single `[` followed by one primitive type char. These are boot-loaded
/// (single loader), so exact-name duplicate rows can be folded safely.
fn is_prim_array_desc(name: &str) -> bool {
    name.len() == 2
        && name.as_bytes()[0] == b'['
        && matches!(
            name.as_bytes()[1],
            b'Z' | b'C' | b'F' | b'D' | b'S' | b'I' | b'J' | b'B'
        )
}

/// Build a per-class-row remap that folds exact-raw-name duplicate histogram
/// rows into a single canonical (lowest-indexed) row, matching MAT's
/// by-object-type histogram semantics.
///
/// Two name families produce duplicate rows that MAT reports as one:
///  - `java/lang/Class`: class objects (`kind==3`) key under a single sentinel
///    row (`JLC_KEY`), but primitive-type Class *mirrors* (`int.class`, …) are
///    parsed as plain instances whose class-object address *is*
///    `java/lang/Class`, landing in a separate same-named row.
///  - primitive-array descriptors (`[B`, `[I`, …): the actual `byte[]`/`int[]`
///    INSTANCES key under `PRIM_KEY_BASE|type_code`, but the instance-less
///    primitive-array CLASS objects (root-attached to mirror MAT's
///    addSystemClassRootsIfMissing) become reachable metadata objects that
///    intern into a *separate* zero-instance row with the same `[B`/`[I` name.
///
/// Only these two families are folded by name. Ordinary instance rows are
/// interned by loader-distinct class-object address, so a class loaded by two
/// loaders legitimately yields two same-name rows that MUST stay separate; we
/// therefore never fold arbitrary same-name rows.
///
/// The returned vector maps `row -> canonical_row`; non-foldable rows map to
/// themselves. Applying it in the histogram and Biggest-Classes tallies
/// re-attributes the duplicates without touching reachability,
/// `classes_loaded`, or `total_objects`.
fn class_row_remap(g: &Graph) -> Vec<u32> {
    let class_count = g.class_names.len();
    let mut remap: Vec<u32> = (0..class_count as u32).collect();
    // First occurrence of each foldable name becomes its canonical row.
    let mut canonical: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    for (row, name) in g.class_names.iter().enumerate() {
        if name == "java/lang/Class" || is_prim_array_desc(name) {
            let canon = *canonical.entry(name.as_str()).or_insert(row as u32);
            remap[row] = canon;
        }
    }
    remap
}

// ── Formatting helpers ─────────────────────────────────────────────────────

/// ISO-8601 UTC timestamp matching java.time.Instant.toString() shape.
/// Non-deterministic — parity comparison ignores this line.
pub fn now_iso8601() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format_epoch_nanos(now.as_secs(), now.subsec_nanos())
}

/// Format a millis-since-Unix-epoch instant as `YYYY-MM-DDTHH:MM:SSZ` (UTC),
/// second granularity. Used for the deterministic dump-creation timestamp;
/// negative (pre-1970) values are clamped to the epoch.
pub fn format_epoch_ms(ms: i64) -> String {
    let secs = if ms < 0 { 0 } else { (ms / 1000) as u64 };
    let full = format_epoch_nanos(secs, 0);
    // full is "...SS.000000000Z"; trim the fractional seconds for readability.
    match (full.find('.'), full.rfind('Z')) {
        (Some(dot), Some(z)) if dot < z => format!("{}{}", &full[..dot], &full[z..]),
        _ => full,
    }
}

/// Core civil-date formatter (Howard Hinnant's algorithm) shared by the
/// now/creation timestamp helpers. Produces `YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ`.
fn format_epoch_nanos(secs: u64, nanos: u32) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        year, m, d, hh, mm, ss, nanos
    )
}

pub fn format_bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{} B", n);
    }
    if n < 1024 * 1024 {
        return format!("{:.1} KB", n as f64 / 1024.0);
    }
    if n < 1024 * 1024 * 1024 {
        return format!("{:.1} MB", n as f64 / (1024.0 * 1024.0));
    }
    format!("{:.2} GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
}

fn fmt_count(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

pub fn pretty_class_name(raw: &str) -> String {
    if raw.is_empty() {
        return raw.to_string();
    }
    if !raw.starts_with('[') {
        return raw.replace('/', ".");
    }

    let dims = raw.chars().take_while(|&c| c == '[').count();
    let rest = &raw[dims..];

    let base = if rest.len() == 1 {
        match rest.chars().next().unwrap() {
            'Z' => "boolean",
            'B' => "byte",
            'C' => "char",
            'S' => "short",
            'I' => "int",
            'J' => "long",
            'F' => "float",
            'D' => "double",
            _ => rest,
        }
        .to_string()
    } else if rest.starts_with('L') && rest.ends_with(';') {
        rest[1..rest.len() - 1].replace('/', ".")
    } else {
        rest.replace('/', ".")
    };

    format!("{}{}", base, "[]".repeat(dims))
}

/// The full dotted PACKAGE PATH of a class from its JVM internal name.
///
/// Normalises like the histogram (strip leading `[`, strip `L...;`), then takes
/// everything BEFORE the final `.`. Primitives/arrays collapse to the sentinel
/// `(primitives)`; a class in the default package (no dot) becomes `(default)`.
/// Examples: `java/util/concurrent/Foo` -> `java.util.concurrent`;
/// `Foo` -> `(default)`; `[I` -> `(primitives)`.
fn package_path(name: &str) -> String {
    let mut s = name;
    while s.starts_with('[') {
        s = &s[1..];
    }
    if s.starts_with('L') && s.ends_with(';') {
        s = &s[1..s.len() - 1];
    }
    if s.is_empty() || matches!(s, "B" | "C" | "D" | "F" | "I" | "J" | "S" | "Z") {
        return "(primitives)".to_string();
    }
    if s.ends_with("[]") {
        return "(primitives)".to_string();
    }
    let s = s.replace('/', ".");
    match s.rfind('.') {
        Some(dot) => s[..dot].to_string(),
        None => "(default)".to_string(),
    }
}

// ── Data model ──────────────────────────────────────────────────────────────

const THRESHOLD_PCT: f64 = 10.0;
const TOP_N: usize = 20;
/// MAT 1%-of-total pruning threshold for the package tree, in basis points.
const PACKAGE_THRESHOLD_BP: u32 = 100;
/// If the single largest suspect retains at least this share of the reachable
/// heap, the OOM-triage lead-in calls the heap "dominated" by one retainer.
const CONCENTRATION_PCT: f64 = 50.0;

/// One row of the System-Overview class histogram (full, one row per class).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct HistRow {
    pub pretty_class: String,
    pub instances: u64,
    pub shallow: u64,
    pub retained: u64,
}

/// Aggregates for the "System Overview" section.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SystemOverview {
    pub source_name: String,
    /// Full path the dump was opened from (superset of `source_name`).
    pub file_path: String,
    pub format: String,
    pub file_size: u64,
    /// HPROF identifier size in BITS (id_size bytes * 8: 32 or 64).
    pub identifier_size_bits: u32,
    /// Whether the JVM used compressed ordinary object pointers: true when
    /// references are narrower than identifiers (8-byte id, 4-byte ref). None
    /// when undeterminable. Not applicable (false) for 32-bit dumps.
    pub compressed_oops: Option<bool>,
    /// Dump creation time in millis since Unix epoch (HPROF header base
    /// timestamp). None when the header timestamp is absent/zero.
    pub dump_creation: Option<i64>,
    pub total_objects: u64,
    pub total_shallow: u64,
    pub gc_roots: u64,
    pub classes_loaded: u64,
    pub unreachable_count: u64,
    pub unreachable_shallow: u64,
    pub histogram: Vec<HistRow>,
    /// Number of histogram rows the full histogram was capped to, or None when
    /// the histogram is complete (never truncated). Always None today.
    pub histogram_truncated_to: Option<u64>,
}

/// One step of a single-suspect accumulation path.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct PathStep {
    pub depth: usize,
    pub obj_index_1based: usize,
    pub display_class: String,
    pub retained: u64,
}

/// One leak suspect (single large object or class group).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Suspect {
    pub is_single: bool,
    pub pretty_class: String,
    pub instance_count: u64,
    pub retained: u64,
    pub shallow: u64,
    /// Non-empty only for single suspects.
    pub path: Vec<PathStep>,
}

/// Aggregates for the "Leak Suspects" section.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct LeakSuspects {
    pub total_shallow: u64,
    pub suspects: Vec<Suspect>,
}

/// One row of "Biggest Objects".
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ObjRow {
    pub obj_index_1based: usize,
    pub display_class: String,
    pub shallow: u64,
    pub retained: u64,
    /// Retained share of total reachable shallow heap, in integer basis
    /// points (bp = round(retained / total_shallow * 10000)). Deterministic
    /// integer for JSON output; the Markdown renderer uses `pct` instead.
    pub pct_bp: u64,
    /// Retained share as a percentage (0..=100), used only for Markdown
    /// formatting. Skipped from JSON/schema because f64 is a
    /// determinism/precision risk in the machine-readable output.
    #[serde(skip)]
    #[schemars(skip)]
    pub pct: f64,
}

/// One row of "Biggest Classes".
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ClassRow {
    pub pretty_class: String,
    pub instances: u64,
    pub retained: u64,
}

/// One node of the pruned package tree (MAT PackageTreeResult parity).
/// Totals are CUMULATIVE over all top-level dominators in this node's subtree.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct PackageNode {
    /// This segment's name (e.g. "util"); the root node's name is "".
    pub name: String,
    /// Number of top-level dominators under this node (MAT "# Objects").
    pub top_dominator_count: u64,
    /// Sum of shallow heap of the top-level dominators under this node.
    pub shallow_heap: u64,
    /// Cumulative retained heap (sum over the top-level dominators under this node).
    pub retained_heap: u64,
    /// Children sorted retained-desc, tie-broken by name-asc.
    pub children: Vec<PackageNode>,
}

/// Aggregates for the "Top Consumers" section.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TopConsumers {
    pub biggest_objects: Vec<ObjRow>,
    pub biggest_classes: Vec<ClassRow>,
    /// MAT 1%-of-total pruning threshold in basis points (100 bp = 1%).
    pub threshold_bp: u32,
    /// Root of the pruned package tree (root name = "").
    pub biggest_packages: PackageNode,
}

/// Schema version for the machine-readable JSON output. Bump on any
/// breaking change to the `Report` shape; the JSON always carries this.
pub const SCHEMA_VERSION: u32 = 1;

/// Full report data model: only bounded aggregates, never a per-object Vec.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Report {
    pub schema_version: u32,
    pub generated: String,
    pub overview: SystemOverview,
    pub leaks: LeakSuspects,
    pub top: TopConsumers,
}

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
pub fn build_model(g: &Graph, dc_offsets: &[u32], dc_targets: &[u32]) -> Report {
    let generated = now_iso8601();
    crate::trace::probe("build_model: before system_overview aggregates");
    let overview = build_system_overview(g);
    crate::trace::probe("build_model: after system_overview aggregates");
    let leaks = build_leak_suspects(g, dc_offsets, dc_targets);
    crate::trace::probe("build_model: after leak_suspects aggregates");
    let top = build_top_consumers(g);
    crate::trace::probe("build_model: after top_consumers aggregates");
    Report {
        schema_version: SCHEMA_VERSION,
        generated,
        overview,
        leaks,
        top,
    }
}

fn build_system_overview(g: &Graph) -> SystemOverview {
    let n = g.n;
    let undef = u32::MAX;

    // Count reachable objects and total shallow; track unreachable in the same loop.
    let mut total_objects: u64 = 0;
    let mut total_shallow: u64 = 0;
    let mut unreachable_count: u64 = 0;
    let mut unreachable_shallow: u64 = 0;
    for i in 0..n {
        if g.idom[i] != undef {
            total_objects += 1;
            total_shallow += g.shallow[i] as u64;
        } else {
            unreachable_count += 1;
            unreachable_shallow += g.shallow[i] as u64;
        }
    }

    let gc_roots = (g
        .gc_root_indices
        .len()
        .saturating_sub(g.synthetic_root_count)) as u64;
    // Count reachable class-dump objects (objects that ARE Java classes, with defined idom)
    let undef_u32 = u32::MAX;
    let classes_loaded = (0..n)
        .filter(|&i| class_obj_repr(g, i) != u32::MAX && g.idom[i] != undef_u32)
        .count() as u64;

    // TEMP DEBUG (env-gated, inert by default): dump reachable class-object
    // indices so they can be joined against the pass2 index->addr file.
    if std::env::var_os("EXP_DUMP_CLASS_ADDRS").is_some() {
        use std::io::Write as _;
        if let Ok(f) = std::fs::File::create("/tmp/ours_reachable_class_idx.txt") {
            let mut w = std::io::BufWriter::new(f);
            for i in 0..n {
                if class_obj_repr(g, i) != u32::MAX && g.idom[i] != undef_u32 {
                    let _ = writeln!(w, "{}", i);
                }
            }
        }
    }

    // Class histogram: per-class instance count, shallow total, retained total
    let class_count = g.class_names.len();
    let mut inst_count: Vec<u64> = vec![0; class_count];
    let mut shallow_total: Vec<u64> = vec![0; class_count];
    let mut class_retained: Vec<u64> = vec![0; class_count];

    // Fold duplicate `java/lang/Class` rows (primitive-type Class mirrors are
    // parsed as plain instances in a separate row) into the single canonical
    // row so the histogram counts by object type, matching MAT.
    let remap = class_row_remap(g);

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

    // Sort classes by retained desc, emit the FULL histogram (every class).
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
        })
        .collect();

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
        classes_loaded,
        unreachable_count,
        unreachable_shallow,
        histogram,
        histogram_truncated_to: None,
    }
}

fn build_leak_suspects(g: &Graph, dc_offsets: &[u32], dc_targets: &[u32]) -> LeakSuspects {
    let n = g.n;
    let undef = u32::MAX;

    // Total shallow heap of reachable objects
    let total_shallow: u64 = (0..n)
        .filter(|&i| g.idom[i] != undef)
        .map(|i| g.shallow[i] as u64)
        .sum();

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

    // Materialise into the model, resolving the accumulation path for singles.
    let out: Vec<Suspect> = suspects
        .iter()
        .map(|s| {
            let mut path: Vec<PathStep> = Vec::new();
            if s.is_single {
                let mut cur = s.obj_idx as usize;
                for depth in 0..=5 {
                    let ci = g.class_idx[cur] as usize;
                    // For class objects, show the class they represent (MAT
                    // parity: no "class " prefix)
                    let display_class = if class_obj_repr(g, cur) != u32::MAX {
                        let repr = class_obj_repr(g, cur) as usize;
                        if repr < g.class_names.len() {
                            pretty_class_name(&g.class_names[repr])
                        } else {
                            pretty_class_name(&g.class_names[ci])
                        }
                    } else if ci < g.class_names.len() {
                        pretty_class_name(&g.class_names[ci])
                    } else {
                        String::from("?")
                    };

                    path.push(PathStep {
                        depth,
                        obj_index_1based: cur + 1,
                        display_class,
                        retained: g.retained[cur],
                    });

                    // Find child with max retained
                    let best_child = dom_children(cur)
                        .iter()
                        .max_by_key(|&&c| g.retained[c as usize]);
                    match best_child {
                        Some(&c) => cur = c as usize,
                        None => break,
                    }
                }
            }
            Suspect {
                is_single: s.is_single,
                pretty_class: pretty_class_name(&g.class_names[s.class_idx]),
                instance_count: s.instance_count,
                retained: s.retained,
                shallow: s.shallow,
                path,
            }
        })
        .collect();

    LeakSuspects {
        total_shallow,
        suspects: out,
    }
}

fn build_top_consumers(g: &Graph) -> TopConsumers {
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

    // Biggest Objects
    let biggest_objects: Vec<ObjRow> = sorted_top
        .iter()
        .take(TOP_N)
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
        .take(TOP_N)
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
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Render a `Report` into Markdown. Byte-identical to the previous
/// `system_overview` + `leak_suspects` + `top_consumers` concatenation.
pub fn render_markdown(r: &Report) -> String {
    let mut out = String::new();
    render_title(&r.overview, &r.generated, &mut out);
    render_oom_triage(r, &mut out);
    render_system_overview(&r.overview, &mut out);
    render_leak_suspects(&r.leaks, &mut out);
    render_top_consumers(&r.top, r.leaks.total_shallow, &mut out);
    out
}

/// Emit the document title + generation timestamp + horizontal rule.
/// Split out of `render_system_overview` so the OOM-triage lead-in can sit
/// between the title and the first section.
fn render_title(o: &SystemOverview, generated: &str, out: &mut String) {
    out.push_str(&format!("# Heap Dump Analysis: `{}`\n\n", o.source_name));
    out.push_str(&format!(
        "*Generated by hprof-redact views — {}*\n\n",
        generated
    ));
    out.push_str("----\n\n");
}

/// OOM-triage lead-in: a short, human-readable summary re-projecting data
/// already in the model (no new model fields). Names the dominant retainer
/// and characterises how concentrated retention is. Pure function of `Report`.
fn render_oom_triage(r: &Report, out: &mut String) {
    out.push_str("## OOM Triage\n\n");
    out.push_str("_Where the reachable heap is concentrated, at a glance._\n\n");

    // Percentage basis matches the existing tables: retained / total reachable
    // shallow heap. Reuse the leak-suspects total (identical to top-consumers').
    let total = r.leaks.total_shallow;
    let pct_of = |retained: u64| -> f64 {
        if total > 0 {
            retained as f64 / total as f64 * 100.0
        } else {
            0.0
        }
    };

    // Headline retainer: prefer the #1 leak suspect; fall back to the biggest
    // top-level consumer; otherwise report that nothing dominates.
    if let Some(s) = r.leaks.suspects.first() {
        let kind = if s.is_single {
            "a single object"
        } else {
            "a class group"
        };
        out.push_str(&format!(
            "- **Headline retainer:** `{}` ({}) retains {} ({:.1}% of reachable heap).\n",
            s.pretty_class,
            kind,
            format_bytes(s.retained),
            pct_of(s.retained),
        ));
    } else if let Some(o) = r.top.biggest_objects.first() {
        out.push_str(&format!(
            "- **Headline retainer:** `{}` (object #{}) retains {} ({:.1}% of reachable heap).\n",
            o.display_class,
            o.obj_index_1based,
            format_bytes(o.retained),
            pct_of(o.retained),
        ));
    } else {
        out.push_str("- **Headline retainer:** No dominant retainer found.\n");
    }

    // Concentration hint: derived purely from the suspects list.
    match r.leaks.suspects.first() {
        Some(s) if pct_of(s.retained) >= CONCENTRATION_PCT => {
            out.push_str(&format!(
                "- **Concentration:** A single object/class group dominates the heap ({:.1}%).\n",
                pct_of(s.retained),
            ));
        }
        Some(_) => {
            out.push_str("- **Concentration:** Retention is spread across multiple roots.\n");
        }
        None => {
            out.push_str(
                "- **Concentration:** No suspect exceeds the threshold; retention is spread across many roots.\n",
            );
        }
    }
    out.push('\n');
}

fn render_system_overview(o: &SystemOverview, out: &mut String) {
    out.push_str("## System Overview\n\n");
    out.push_str("_Reachable-heap totals and the largest classes by retained heap._\n\n");
    out.push_str("### Heap Summary\n\n");
    out.push_str("| Property | Value |\n");
    out.push_str("|---|---|\n");
    out.push_str(&format!("| HPROF format | {} |\n", o.format));
    out.push_str(&format!("| File size | {} |\n", format_bytes(o.file_size)));
    out.push_str(&format!(
        "| Identifier size | {}-bit |\n",
        o.identifier_size_bits
    ));
    if let Some(coops) = o.compressed_oops {
        out.push_str(&format!(
            "| Compressed OOPs | {} |\n",
            if coops { "yes" } else { "no" }
        ));
    }
    if let Some(ms) = o.dump_creation {
        out.push_str(&format!("| Dump created | {} |\n", format_epoch_ms(ms)));
    }
    out.push_str(&format!(
        "| Total objects | {} |\n",
        fmt_count(o.total_objects)
    ));
    out.push_str(&format!(
        "| Total shallow heap | {} |\n",
        format_bytes(o.total_shallow)
    ));
    out.push_str(&format!("| GC roots | {} |\n", fmt_count(o.gc_roots)));
    out.push_str(&format!(
        "| Classes loaded | {} |\n",
        fmt_count(o.classes_loaded)
    ));
    if o.unreachable_count > 0 {
        out.push_str(&format!(
            "| Unreachable objects (excluded) | {} ({}) |\n",
            fmt_count(o.unreachable_count),
            format_bytes(o.unreachable_shallow),
        ));
    }
    out.push('\n');

    out.push_str("### Class Histogram (by Retained Heap)\n\n");
    out.push_str("| # | Class | Instances | Shallow Heap | Retained Heap |\n");
    out.push_str("|---|---|---:|---:|---:|\n");
    // The model carries the FULL histogram; the Markdown view shows the top 50
    // rows for readability. The complete data lives in the JSON output.
    for (rank, row) in o.histogram.iter().take(50).enumerate() {
        out.push_str(&format!(
            "| {} | `{}` | {} | {} | {} |\n",
            rank + 1,
            row.pretty_class,
            fmt_count(row.instances),
            format_bytes(row.shallow),
            fmt_count(row.retained),
        ));
    }
    out.push('\n');
}

fn render_leak_suspects(l: &LeakSuspects, out: &mut String) {
    out.push_str("## Leak Suspects\n\n");

    if l.suspects.is_empty() {
        out.push_str("No single object or class group exceeds the threshold.\n\n");
        return;
    }

    for (rank, s) in l.suspects.iter().enumerate() {
        let pct = if l.total_shallow > 0 {
            s.retained as f64 / l.total_shallow as f64 * 100.0
        } else {
            0.0
        };
        let type_label = if s.is_single {
            "Single large object"
        } else {
            "Class group"
        };

        out.push_str(&format!(
            "### Suspect {}: `{}`\n\n",
            rank + 1,
            s.pretty_class
        ));
        out.push_str(&format!("- **Type**: {}\n", type_label));
        out.push_str(&format!(
            "- **Instances**: {}\n",
            fmt_count(s.instance_count)
        ));
        out.push_str(&format!(
            "- **Retained heap**: {} ({:.1}% of total)\n",
            format_bytes(s.retained),
            pct
        ));
        out.push_str(&format!(
            "- **Shallow heap**: {}\n",
            format_bytes(s.shallow)
        ));
        out.push('\n');

        // Accumulation path for single suspects
        if s.is_single {
            out.push_str("**Accumulation point path** (largest retained child at each step):\n\n");
            out.push_str("| Depth | Object Index | Class | Retained |\n");
            out.push_str("|---|---|---|---:|\n");

            for step in &s.path {
                out.push_str(&format!(
                    "| {} | {} | `{}` | {} |\n",
                    step.depth,
                    step.obj_index_1based,
                    step.display_class,
                    format_bytes(step.retained),
                ));
            }
            out.push('\n');
        }
    }
}

fn render_top_consumers(t: &TopConsumers, total_shallow: u64, out: &mut String) {
    out.push_str("## Top Consumers\n\n");
    out.push_str("### Biggest Objects (Top-Level Dominators)\n\n");
    out.push_str("| # | Object Index | Class | Shallow | Retained |\n");
    out.push_str("|---|---|---|---:|---:|\n");

    for (rank, row) in t.biggest_objects.iter().enumerate() {
        out.push_str(&format!(
            "| {} | {} | `{}` | {} | {} ({:.1}%) |\n",
            rank + 1,
            row.obj_index_1based,
            row.display_class,
            format_bytes(row.shallow),
            format_bytes(row.retained),
            if total_shallow > 0 {
                row.retained as f64 / total_shallow as f64 * 100.0
            } else {
                0.0
            },
        ));
    }
    out.push('\n');

    out.push_str("### Biggest Classes by Retained Heap\n\n");
    out.push_str("| # | Class | Instances | Retained Heap |\n");
    out.push_str("|---|---|---:|---:|\n");
    for (rank, row) in t.biggest_classes.iter().enumerate() {
        out.push_str(&format!(
            "| {} | `{}` | {} | {} |\n",
            rank + 1,
            row.pretty_class,
            fmt_count(row.instances),
            format_bytes(row.retained),
        ));
    }
    out.push('\n');

    out.push_str("### Biggest Packages by Retained Heap\n\n");
    out.push_str("| Package | Objects | Shallow | Retained |\n");
    out.push_str("|---|---:|---:|---:|\n");
    if t.biggest_packages.children.is_empty() {
        out.push_str("_No package retains more than 1% of the total retained heap._\n");
        out.push('\n');
        return;
    }
    // Pre-order DFS; the displayed name is the full dotted path accumulated
    // down from the root, so each row is self-describing (no tree-drawing chars).
    fn emit_node(node: &PackageNode, prefix: &str, out: &mut String) {
        let full = if prefix.is_empty() {
            node.name.clone()
        } else {
            format!("{}.{}", prefix, node.name)
        };
        out.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            full,
            fmt_count(node.top_dominator_count),
            format_bytes(node.shallow_heap),
            format_bytes(node.retained_heap),
        ));
        for child in &node.children {
            emit_node(child, &full, out);
        }
    }
    // Skip the synthetic root (name ""); start emitting at its children.
    for child in &t.biggest_packages.children {
        emit_node(child, "", out);
    }
    out.push('\n');
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass2::Graph;
    use std::collections::HashMap;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn test_fmt_count() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1000), "1,000");
        assert_eq!(fmt_count(1_000_000), "1,000,000");
        assert_eq!(fmt_count(2_698_510), "2,698,510");
    }

    #[test]
    fn test_pretty_class_name() {
        assert_eq!(pretty_class_name("java/lang/String"), "java.lang.String");
        assert_eq!(pretty_class_name("[I"), "int[]");
        assert_eq!(pretty_class_name("[B"), "byte[]");
        assert_eq!(
            pretty_class_name("[Ljava/lang/String;"),
            "java.lang.String[]"
        );
        assert_eq!(pretty_class_name("[[I"), "int[][]");
        assert_eq!(pretty_class_name("[Z"), "boolean[]");
        assert_eq!(pretty_class_name("[C"), "char[]");
    }

    #[test]
    fn test_package_path() {
        assert_eq!(
            package_path("java/util/concurrent/Foo"),
            "java.util.concurrent"
        );
        assert_eq!(package_path("Foo"), "(default)");
        assert_eq!(package_path("[I"), "(primitives)");
        assert_eq!(package_path("[B"), "(primitives)");
        assert_eq!(package_path("java/lang/String"), "java.lang");
        assert_eq!(package_path("[Ljava/lang/String;"), "java.lang");
        assert_eq!(
            package_path("java/util/concurrent/ConcurrentHashMap$Node"),
            "java.util.concurrent"
        );
    }

    /// Build a tiny synthetic Graph plus the dominator-children CSR that
    /// `build_model` expects, for the objects/classes described below.
    ///
    /// Layout (n objects, vroot = n):
    /// - `idom[i]`  = immediate dominator (u32::MAX = unreachable, n = vroot).
    /// - `class_idx[i]` = class-histogram row for object i.
    /// - `shallow[i]`, `retained[i]` as given.
    /// - `class_names` gives the raw JVM names for each class row.
    /// - `class_obj_class_idx` maps class-object index -> represented class row.
    /// - `has_same[i]` marks objects with a same-class ancestor (excluded from
    ///   class_retained accumulation).
    ///
    /// Returns `(Graph, dc_off, dc_tgt)`.
    #[allow(clippy::too_many_arguments)]
    fn make_graph(
        idom: Vec<u32>,
        class_idx: Vec<u32>,
        shallow: Vec<u32>,
        retained: Vec<u64>,
        class_names: Vec<&str>,
        class_obj: &[(u32, u32)],
        has_same_true: &[usize],
        gc_root_indices: Vec<u32>,
        synthetic_root_count: usize,
    ) -> (Graph, Vec<u32>, Vec<u32>) {
        let n = idom.len();
        let mut class_obj_class_idx: HashMap<u32, u32> = HashMap::new();
        for &(k, v) in class_obj {
            class_obj_class_idx.insert(k, v);
        }
        let mut has_same = crate::bitset::Bitset::with_len(n);
        for &i in has_same_true {
            has_same.set(i);
        }

        // Build dominator-children CSR indexed 0..=n (node n = vroot). Children
        // of node p are all i with idom[i] == p, in ascending object order.
        let mut children: Vec<Vec<u32>> = vec![Vec::new(); n + 1];
        for (i, &d) in idom.iter().enumerate() {
            if d == u32::MAX {
                continue;
            }
            children[d as usize].push(i as u32);
        }
        let mut dc_off: Vec<u32> = Vec::with_capacity(n + 2);
        let mut dc_tgt: Vec<u32> = Vec::new();
        dc_off.push(0);
        for kids in &children {
            dc_tgt.extend_from_slice(kids);
            dc_off.push(dc_tgt.len() as u32);
        }

        let gc_root_types: Vec<u8> = vec![crate::types::heap::ROOT_UNKNOWN; gc_root_indices.len()];
        let g = Graph {
            n,
            format: "JAVA PROFILE 1.0.2".to_string(),
            file_size: 4096,
            source_name: "test.hprof".to_string(),
            // Full path superset of source_name; compressed-oops fixture: 8-byte
            // ids, 4-byte refs. header_timestamp_ms = 1_700_000_000_000
            // (2023-11-14T22:13:20Z).
            file_path: "/tmp/dumps/test.hprof".to_string(),
            id_size: 8,
            ref_size: 4,
            header_timestamp_ms: 1_700_000_000_000,
            gc_root_indices,
            gc_root_types,
            shallow,
            class_idx,
            class_names: class_names.iter().map(|s| s.to_string()).collect(),
            class_obj_class_idx,
            fwd_offsets: Vec::new(),
            fwd_targets: Vec::new(),
            synthetic_root_count,
            idom,
            retained,
            has_same_class_ancestor: has_same,
        };
        (g, dc_off, dc_tgt)
    }

    /// A fixture with 4 reachable objects + 1 unreachable, 3 classes.
    /// - obj0: class0 (com/foo/A), top-level, retained 1000, shallow 100
    /// - obj1: class1 (com/foo/B), top-level, retained 1000, shallow 100 (ties obj0)
    /// - obj2: class0 (com/foo/A), dominated by obj0, retained 50, shallow 50, has_same
    /// - obj3: class2 (org/bar/C), top-level, retained 200, shallow 20
    /// - obj4: class1, UNREACHABLE (idom = MAX), shallow 7
    /// - vroot = 5.
    fn fixture() -> (Graph, Vec<u32>, Vec<u32>) {
        make_graph(
            vec![5, 5, 0, 5, u32::MAX],   // idom
            vec![0, 1, 0, 2, 1],          // class_idx
            vec![100, 100, 50, 20, 7],    // shallow
            vec![1000, 1000, 50, 200, 0], // retained
            vec!["com/foo/A", "com/foo/B", "org/bar/C"],
            &[],           // no class objects
            &[2],          // obj2 has same-class ancestor
            vec![0, 1, 3], // gc roots
            0,             // no synthetic roots
        )
    }

    #[test]
    fn test_build_model_system_overview() {
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model(&g, &dc_off, &dc_tgt);
        let o = &r.overview;
        assert_eq!(o.total_objects, 4);
        assert_eq!(o.total_shallow, 100 + 100 + 50 + 20);
        assert_eq!(o.unreachable_count, 1);
        assert_eq!(o.unreachable_shallow, 7);
        assert_eq!(o.gc_roots, 3);
        assert_eq!(o.classes_loaded, 0);

        // Phase D System Overview cheap fields.
        assert_eq!(o.identifier_size_bits, 64); // id_size 8 bytes * 8
        assert_eq!(o.compressed_oops, Some(true)); // ref_size 4 < id_size 8
        assert_eq!(o.dump_creation, Some(1_700_000_000_000));
        assert_eq!(o.file_path, "/tmp/dumps/test.hprof");
        assert_eq!(o.histogram_truncated_to, None);

        // Histogram: class0 retained = obj0(1000) + obj2 excluded (has_same) = 1000
        //            class1 retained = obj1(1000) = 1000
        //            class2 retained = obj3(200) = 200
        // Sort by retained desc, tie-break ascending class index -> class0, class1, class2.
        assert_eq!(o.histogram.len(), 3);
        assert_eq!(o.histogram[0].pretty_class, "com.foo.A");
        assert_eq!(o.histogram[0].retained, 1000);
        assert_eq!(o.histogram[0].instances, 2); // obj0 + obj2
        assert_eq!(o.histogram[0].shallow, 150);
        assert_eq!(o.histogram[1].pretty_class, "com.foo.B");
        assert_eq!(o.histogram[1].retained, 1000);
        assert_eq!(o.histogram[2].pretty_class, "org.bar.C");
        assert_eq!(o.histogram[2].retained, 200);
    }

    /// Regression: MAT counts the class histogram BY OBJECT TYPE, so every
    /// `java/lang/Class`-typed object must land in the single `java/lang/Class`
    /// row — including primitive-type Class mirrors (`int.class`, `void.class`,
    /// …) that HPROF stores as plain instances in a *separate* histogram row
    /// that is also named `java/lang/Class`. This test builds both a real class
    /// object and such a mirror instance and asserts they are counted together,
    /// while `classes_loaded` (distinct CLASS_DUMP objects) stays unchanged and
    /// an unrelated class is not miscounted.
    #[test]
    fn test_histogram_folds_duplicate_java_lang_class_rows() {
        // Rows: 0 = java/lang/Class (canonical, used by the class object),
        //       1 = com/foo/A (a normal class),
        //       2 = java/lang/Class (duplicate row: the primitive mirror lands
        //           here because it is a plain instance keyed by the
        //           java/lang/Class class-object address).
        // Objects:
        //   obj0: class_idx 0, IS a class object (represents row 1), top-level.
        //   obj1: class_idx 1, normal instance of com/foo/A, top-level.
        //   obj2: class_idx 2, java/lang/Class-typed mirror, NOT a class object.
        let (g, dc_off, dc_tgt) = make_graph(
            vec![3, 3, 3],        // idom (vroot = 3)
            vec![0, 1, 2],        // class_idx
            vec![100, 50, 20],    // shallow
            vec![1000, 500, 200], // retained
            vec!["java/lang/Class", "com/foo/A", "java/lang/Class"],
            &[(0, 1)], // obj0 is a class object representing row 1
            &[],       // none excluded from retained accumulation
            vec![0, 1, 2],
            0,
        );
        let r = build_model(&g, &dc_off, &dc_tgt);
        let o = &r.overview;

        // classes_loaded counts distinct CLASS_DUMP objects (class_obj_repr set)
        // — only obj0. The fold must NOT change this.
        assert_eq!(o.classes_loaded, 1);
        assert_eq!(o.total_objects, 3);

        // Exactly ONE java.lang.Class histogram row, counting BOTH the class
        // object (obj0) and the primitive mirror (obj2).
        let jlc_rows: Vec<&HistRow> = o
            .histogram
            .iter()
            .filter(|h| h.pretty_class == "java.lang.Class")
            .collect();
        assert_eq!(
            jlc_rows.len(),
            1,
            "duplicate java.lang.Class rows not folded"
        );
        assert_eq!(
            jlc_rows[0].instances, 2,
            "mirror not counted under java.lang.Class"
        );
        // Shallow of both mirror + class object moved into the folded row.
        assert_eq!(jlc_rows[0].shallow, 120);

        // The unrelated class is not miscounted.
        let a_row = o
            .histogram
            .iter()
            .find(|h| h.pretty_class == "com.foo.A")
            .expect("com.foo.A row present");
        assert_eq!(a_row.instances, 1);

        // Biggest Classes (over top-level dominators) also folds by type.
        let jlc_big: Vec<&ClassRow> = r
            .top
            .biggest_classes
            .iter()
            .filter(|c| c.pretty_class == "java.lang.Class")
            .collect();
        assert_eq!(jlc_big.len(), 1);
        assert_eq!(jlc_big[0].instances, 2);
    }

    #[test]
    fn test_format_epoch_ms_edges() {
        // Negative (pre-1970) inputs clamp to the epoch, identical to ms == 0.
        assert_eq!(format_epoch_ms(-1), format_epoch_ms(0));
        assert_eq!(format_epoch_ms(0), "1970-01-01T00:00:00Z");
        // A known non-zero instant renders the expected second-granularity ISO.
        assert_eq!(format_epoch_ms(1_700_000_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn test_system_overview_uncompressed_and_no_timestamp() {
        // Same fixture, but override the two header-derived fields to cover the
        // OTHER branches: ref_size == id_size (no compressed oops) and a zero
        // header timestamp (no dump-creation instant).
        let (mut g, dc_off, dc_tgt) = fixture();
        g.ref_size = g.id_size; // 8 == 8 -> not compressed
        g.header_timestamp_ms = 0; // no creation timestamp
        let r = build_model(&g, &dc_off, &dc_tgt);
        let o = &r.overview;
        assert_eq!(o.compressed_oops, Some(false)); // ref_size == id_size
        assert_eq!(o.dump_creation, None); // header_timestamp_ms == 0
    }

    #[test]
    fn test_build_model_top_consumers_package_determinism() {
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model(&g, &dc_off, &dc_tgt);
        let t = &r.top;

        // Biggest objects: top-level are obj0(1000), obj1(1000), obj3(200).
        // Tie between obj0/obj1 broken by ascending index -> obj0 (index 1), obj1 (index 2).
        assert_eq!(t.biggest_objects.len(), 3);
        assert_eq!(t.biggest_objects[0].obj_index_1based, 1);
        assert_eq!(t.biggest_objects[1].obj_index_1based, 2);
        assert_eq!(t.biggest_objects[2].obj_index_1based, 4);

        // Biggest classes (over top-level only): class0=1000, class1=1000, class2=200.
        assert_eq!(t.biggest_classes[0].pretty_class, "com.foo.A");
        assert_eq!(t.biggest_classes[1].pretty_class, "com.foo.B");
        assert_eq!(t.biggest_classes[2].pretty_class, "org.bar.C");

        // Biggest packages: tree over full dotted paths.
        //   obj0 com/foo/A -> path com.foo (retained 1000, shallow 100)
        //   obj1 com/foo/B -> path com.foo (retained 1000, shallow 100)
        //   obj3 org/bar/C -> path org.bar (retained 200, shallow 20)
        // Root cumulative: retained 2200, shallow 220, count 3.
        let root = &t.biggest_packages;
        assert_eq!(root.name, "");
        assert_eq!(root.retained_heap, 2200);
        assert_eq!(root.shallow_heap, 220);
        assert_eq!(root.top_dominator_count, 3);
        // Children sorted retained-desc then name-asc: "com" (2000) before "org" (200).
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].name, "com");
        assert_eq!(root.children[0].retained_heap, 2000);
        assert_eq!(root.children[0].shallow_heap, 200);
        assert_eq!(root.children[0].top_dominator_count, 2);
        assert_eq!(root.children[1].name, "org");
        assert_eq!(root.children[1].retained_heap, 200);
        // Nested path com -> foo carries the cumulative totals of its subtree.
        assert_eq!(root.children[0].children.len(), 1);
        let foo = &root.children[0].children[0];
        assert_eq!(foo.name, "foo");
        assert_eq!(foo.retained_heap, 2000);
        assert_eq!(foo.shallow_heap, 200);
        assert_eq!(foo.top_dominator_count, 2);
        assert!(foo.children.is_empty());
        // threshold_bp is the MAT 1%-of-total marker.
        assert_eq!(t.threshold_bp, 100);
    }

    #[test]
    fn test_build_model_packages_pruning() {
        // Two top-level dominators: a big one (>=1% of total) and a tiny one
        // (<1% of total). The tiny package's whole subtree must be pruned.
        // big: retained 10000 in com/big/Foo; small: retained 1 in org/tiny/Bar.
        // total = 10001; 1% threshold => keep >= 100.06 (i.e. >= floor via bp math).
        let (g, dc_off, dc_tgt) = make_graph(
            vec![2, 2],     // idom: obj0,obj1 both under vroot (node 2)
            vec![0, 1],     // class_idx
            vec![50, 5],    // shallow
            vec![10000, 1], // retained
            vec!["com/big/Foo", "org/tiny/Bar"],
            &[],
            &[],
            vec![0, 1],
            0,
        );
        let r = build_model(&g, &dc_off, &dc_tgt);
        let root = &r.top.biggest_packages;
        // Root keeps cumulative totals over ALL dominators (before pruning).
        assert_eq!(root.retained_heap, 10001);
        assert_eq!(root.top_dominator_count, 2);
        // Only the big package survives pruning.
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].name, "com");
        assert_eq!(root.children[0].retained_heap, 10000);
    }

    #[test]
    fn test_build_model_packages_nothing_over_threshold() {
        // Many equal tiny packages: each is well under 1% of the total, so the
        // root ends up with NO children ("nothing over threshold" case).
        // 200 dominators, each retained 1, in packages pkgN/Foo (all distinct).
        let count = 200usize;
        let idom = vec![count as u32; count]; // all top-level (vroot = node `count`)
        let class_idx: Vec<u32> = (0..count as u32).collect();
        let shallow: Vec<u32> = vec![1; count];
        let retained: Vec<u64> = vec![1; count];
        let names: Vec<String> = (0..count).map(|i| format!("pkg{i}/Foo")).collect();
        let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let gc_roots: Vec<u32> = (0..count as u32).collect();
        let (g, dc_off, dc_tgt) = make_graph(
            idom,
            class_idx,
            shallow,
            retained,
            name_refs,
            &[],
            &[],
            gc_roots,
            0,
        );
        let mut r = build_model(&g, &dc_off, &dc_tgt);
        let root = &r.top.biggest_packages;
        assert_eq!(root.top_dominator_count, count as u64);
        assert!(
            root.children.is_empty(),
            "no single package should exceed 1% of the total"
        );
        r.generated = "FIXED".to_string();
        let md = render_markdown(&r);
        assert!(
            md.contains("_No package retains more than 1% of the total retained heap._"),
            "nothing-over-threshold marker must be rendered"
        );
    }

    #[test]
    fn test_build_model_leak_suspects() {
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model(&g, &dc_off, &dc_tgt);
        let l = &r.leaks;
        // total_shallow = 270, threshold = 27. Singles directly under vroot with
        // retained >= 27: obj0(1000), obj1(1000), obj3(200) all qualify.
        assert_eq!(l.total_shallow, 270);
        assert_eq!(l.suspects.len(), 3);
        // Sorted retained desc, ties by class_idx then obj_idx: obj0(class0),
        // obj1(class1) both 1000 -> class0 first; then obj3.
        assert!(l.suspects[0].is_single);
        assert_eq!(l.suspects[0].pretty_class, "com.foo.A");
        assert_eq!(l.suspects[1].pretty_class, "com.foo.B");
        assert_eq!(l.suspects[2].pretty_class, "org.bar.C");
        // Single suspect must have an accumulation path starting at itself.
        assert!(!l.suspects[0].path.is_empty());
        assert_eq!(l.suspects[0].path[0].depth, 0);
        assert_eq!(l.suspects[0].path[0].obj_index_1based, 1);
    }

    #[test]
    fn test_render_markdown_deterministic() {
        // Build the model twice and assert render output is byte-identical.
        // This specifically guards the Biggest-Packages HashMap sort fix.
        let (g1, off1, tgt1) = fixture();
        let (g2, off2, tgt2) = fixture();
        let mut r1 = build_model(&g1, &off1, &tgt1);
        let mut r2 = build_model(&g2, &off2, &tgt2);
        // Neutralise the nondeterministic timestamp line.
        r1.generated = "FIXED".to_string();
        r2.generated = "FIXED".to_string();
        assert_eq!(render_markdown(&r1), render_markdown(&r2));
    }

    #[test]
    fn test_render_markdown_structure() {
        let (g, dc_off, dc_tgt) = fixture();
        let mut r = build_model(&g, &dc_off, &dc_tgt);
        r.generated = "FIXED".to_string();
        let md = render_markdown(&r);
        assert!(md.starts_with("# Heap Dump Analysis: `test.hprof`\n\n"));
        assert!(md.contains("## System Overview\n\n"));
        assert!(md.contains("### Class Histogram (by Retained Heap)\n\n"));
        assert!(md.contains("## Leak Suspects\n\n"));
        assert!(md.contains("## Top Consumers\n\n"));
        assert!(md.contains("### Biggest Packages by Retained Heap\n\n"));
    }

    #[test]
    fn test_render_markdown_oom_triage() {
        let (g, dc_off, dc_tgt) = fixture();
        let mut r = build_model(&g, &dc_off, &dc_tgt);
        r.generated = "FIXED".to_string();
        let md = render_markdown(&r);

        // (a) new OOM-triage heading + headline retainer line present.
        assert!(
            md.contains("## OOM Triage\n\n"),
            "missing OOM Triage heading"
        );
        assert!(
            md.contains("- **Headline retainer:**"),
            "missing headline retainer line"
        );
        // Fixture's #1 suspect is com.foo.A (a single object) at 1000/270 -> dominates.
        assert!(
            md.contains("`com.foo.A`"),
            "headline should name the #1 suspect"
        );
        assert!(
            md.contains("A single object/class group dominates the heap"),
            "1000/270 is >= 50% so it should read as dominated"
        );

        // The triage block must precede System Overview.
        let tri = md.find("## OOM Triage").unwrap();
        let sys = md.find("## System Overview").unwrap();
        assert!(tri < sys, "OOM Triage must come before System Overview");

        // (b) determinism guard: render twice == identical.
        assert_eq!(md, render_markdown(&r));

        // (c) data-preservation: all key section headings still present.
        for needle in [
            "System Overview",
            "Class Histogram",
            "Leak Suspects",
            "Top Consumers",
            "Biggest Objects",
            "Biggest Classes",
            "Biggest Packages",
        ] {
            assert!(md.contains(needle), "missing section: {needle}");
        }
    }

    // ── Phase B: JSON / schema conformance ─────────────────────────────────

    /// Build the fixture Report with the nondeterministic timestamp neutralised.
    fn fixture_report() -> Report {
        let (g, dc_off, dc_tgt) = fixture();
        let mut r = build_model(&g, &dc_off, &dc_tgt);
        r.generated = "FIXED".to_string();
        r
    }

    #[test]
    fn json_round_trip() {
        let mut r = fixture_report();
        let json = serde_json::to_string(&r).expect("serialize");
        let back: Report = serde_json::from_str(&json).expect("deserialize");
        // ObjRow::pct is #[serde(skip)] (f64 kept out of JSON), so it
        // deserializes to its Default (0.0). Zero it on the original before
        // comparing; every OTHER field must survive the round trip.
        for row in &mut r.top.biggest_objects {
            row.pct = 0.0;
        }
        assert_eq!(r, back, "round-tripped Report must equal the original");
    }

    #[test]
    fn render_markdown_round_trips_through_json() {
        // Proves the --render offline path is faithful: serializing a Report to
        // JSON and deserializing it back must produce byte-identical Markdown.
        let r = fixture_report();
        let json = serde_json::to_string(&r).expect("serialize");
        let back: Report = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            render_markdown(&r),
            render_markdown(&back),
            "render_markdown must be stable across a JSON round trip"
        );
    }

    #[test]
    fn json_serialization_is_deterministic() {
        let r = fixture_report();
        let a = serde_json::to_string_pretty(&r).unwrap();
        let b = serde_json::to_string_pretty(&r).unwrap();
        assert_eq!(
            a, b,
            "serializing the same Report twice must be byte-identical"
        );
    }

    #[test]
    fn json_validates_against_schema() {
        let r = fixture_report();
        let instance = serde_json::to_value(&r).expect("Report -> Value");
        let schema = serde_json::to_value(schemars::schema_for!(Report)).expect("schema -> Value");
        let validator = jsonschema::validator_for(&schema).expect("compile schema (draft 2020-12)");
        assert!(
            validator.validate(&instance).is_ok(),
            "serialized fixture Report must validate against schema_for!(Report)"
        );
    }

    #[test]
    fn emit_schema_matches_committed_file() {
        let committed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/schema/report.schema.json"
            ))
            .expect("read committed schema"),
        )
        .expect("parse committed schema");
        let fresh = serde_json::to_value(schemars::schema_for!(Report)).expect("fresh schema");
        // Value-equality: whitespace / key ordering must not cause false diffs.
        assert_eq!(
            committed, fresh,
            "schema/report.schema.json must equal a fresh schema_for!(Report);              regenerate via `--emit-schema` if the model changed"
        );
    }

    #[test]
    fn schema_version_guard() {
        let r = fixture_report();
        assert_eq!(r.schema_version, SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
