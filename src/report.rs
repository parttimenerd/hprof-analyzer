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

/// Human-readable label for an HPROF GC-root sub-tag, used by the
/// GC-roots-by-type breakdown. Mirrors the MAT root-type naming.
fn gc_root_type_label(ty: u8) -> &'static str {
    use crate::types::heap;
    match ty {
        heap::ROOT_SYSTEM_CLASS => "System Class",
        heap::ROOT_JNI_GLOBAL => "JNI Global",
        heap::ROOT_JNI_LOCAL => "JNI Local",
        heap::ROOT_JAVA_FRAME => "Java Frame",
        heap::ROOT_NATIVE_STACK => "Native Stack",
        heap::ROOT_STICKY_CLASS => "Sticky Class",
        heap::ROOT_THREAD_BLOCK => "Thread Block",
        heap::ROOT_MONITOR_USED => "Busy Monitor",
        heap::ROOT_THREAD_OBJ => "Thread",
        _ => "Unknown",
    }
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

/// Human-readable label for a GC-root HPROF sub-tag (see `types::heap::ROOT_*`).
/// Returns `None` for `ROOT_UNKNOWN` and any unrecognised code, so callers can
/// suppress the "held by" clause when the holding root type is not meaningful.
/// Labels follow MAT's GC-root naming.
fn gc_root_type_label_opt(code: u8) -> Option<&'static str> {
    use crate::types::heap;
    match code {
        heap::ROOT_JNI_GLOBAL => Some("JNI Global"),
        heap::ROOT_JNI_LOCAL => Some("JNI Local"),
        heap::ROOT_JAVA_FRAME => Some("Java Frame"),
        heap::ROOT_NATIVE_STACK => Some("Native Stack"),
        heap::ROOT_STICKY_CLASS => Some("Sticky Class"),
        heap::ROOT_THREAD_BLOCK => Some("Thread Block"),
        heap::ROOT_MONITOR_USED => Some("Busy Monitor"),
        heap::ROOT_THREAD_OBJ => Some("Thread"),
        heap::ROOT_SYSTEM_CLASS => Some("System Class"),
        _ => None,
    }
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

/// The 4-way kind of a reachable object, for heap composition (B5). Derives
/// from class-object membership and the raw JVM class-name descriptor — there
/// is no `kind[]` array in Graph. Mirrors `pretty_class_name`'s array parsing:
/// a single `[X` primitive descriptor is a primitive array; any other `[…`
/// (e.g. `[L…;`, `[[B`) is an object array.
fn object_kind(g: &Graph, i: usize) -> &'static str {
    if class_obj_repr(g, i) != u32::MAX {
        return "Class objects";
    }
    let raw = match g.class_names.get(g.class_idx[i] as usize) {
        Some(r) => r,
        None => return "Instances",
    };
    if is_prim_array_desc(raw) {
        "Primitive arrays"
    } else if raw.starts_with('[') {
        "Object arrays"
    } else {
        "Instances"
    }
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
/// Default per-suspect cap on the "accumulated objects" lists (immediately
/// dominated children + by-class histogram). MAT uses 20; we default higher
/// so more of the retained tail is visible. Overridable via
/// `--leak-children-cap=N`.
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
    /// Class-loader object address that loaded this class (0 = boot loader).
    /// Distinct (class, loader) pairs are distinct rows, matching MAT's
    /// class-object-identity histogram keying.
    pub loader_id: u64,
    /// Human-readable label for `loader_id`: the class NAME of the loader
    /// object (e.g. `jdk/internal/loader/ClassLoaders$AppClassLoader`), or
    /// `<boot>` for the boot loader (address 0). `None` when the loader address
    /// could not be resolved (e.g. leak-suspect `dominated_by_class` rows where
    /// the histogram-row index is not readily available). Purely descriptive —
    /// NOT parity-gated and never compared numerically.
    pub loader_label: Option<String>,
}

/// One row of the GC-roots-by-type breakdown: a human-readable root-type label
/// and how many roots carry that HPROF type.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GcRootTypeRow {
    pub root_type: String,
    pub count: u64,
}

/// One kind-bucket of the heap-composition breakdown (B5).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct KindStat {
    /// One of: "Instances", "Object arrays", "Primitive arrays", "Class objects".
    pub kind: String,
    pub objects: u64,
    pub shallow_heap: u64,
}

/// B5: reachable-heap composition split by object kind (instances vs. arrays
/// vs. class objects). Rows are in fixed kind order; empty buckets are omitted.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct HeapComposition {
    pub by_kind: Vec<KindStat>,
}

/// One bucket of the dominator-depth histogram (B2): how many reachable objects
/// sit exactly `depth` idom-hops below the virtual root.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DepthBucket {
    pub depth: u32,
    pub objects: u64,
}

/// B3: retention concentration over top-level dominators. Basis-point shares
/// (of total reachable shallow heap) held by the top-1/10/100 objects, plus how
/// many single objects each hold >=1% of the heap. Answers "one leak or many?".
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct RetentionSummary {
    pub total_retained: u64,
    /// Retained share of the top-1 / top-10 / top-100 top-level dominators, in
    /// integer basis points (100 bp = 1%) of total reachable shallow heap.
    pub top1_bp: u32,
    pub top10_bp: u32,
    pub top100_bp: u32,
    /// Count of single objects each retaining >=1% of total reachable shallow.
    pub num_objects_ge_1pct: u64,
}

/// One decoded JVM system property (`java.lang.System.props` entry). Serialized
/// as a stable `{ "key": ..., "value": ... }` object (rather than a positional
/// array) so the JSON schema is self-describing.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct PropEntry {
    pub key: String,
    pub value: String,
}

/// F2: per-class-loader rollup over the class histogram. One row per distinct
/// `loader_id`, aggregating the classes it loaded. A bounded reduction over the
/// histogram (row count <= #loaders), so RSS-safe. Sorted retained-desc.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct LoaderRollup {
    /// Human-readable loader label (class name of the loader object, or
    /// `<boot>`); `None` when the label could not be resolved.
    pub loader_label: Option<String>,
    /// Loader object address (0 = boot loader).
    pub loader_id: u64,
    /// Number of distinct classes loaded by this loader.
    pub class_count: u64,
    pub instances: u64,
    pub shallow: u64,
    pub retained: u64,
}

/// F2: a class name loaded under more than one class loader — a classic
/// class-loader-leak signature (the same class re-loaded per web-app reload,
/// per plugin, etc.). Grouped by `pretty_class`; `loaders` is capped.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DuplicateClass {
    pub pretty_class: String,
    /// Number of DISTINCT loader ids this class name appears under (>= 2).
    pub loader_count: u64,
    /// Loader labels (capped) that loaded this class name, for display.
    pub loaders: Vec<String>,
    pub total_instances: u64,
    pub total_retained: u64,
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
    /// GC roots broken down by HPROF root type (e.g. "System Class", "Thread",
    /// "JNI Global"), sorted by count descending then label ascending. Excludes
    /// synthetic roots the analyzer injects. Empty only when there are no roots.
    pub gc_roots_by_type: Vec<GcRootTypeRow>,
    /// B5: reachable-heap composition by object kind. Always present; empty
    /// only for an empty heap.
    pub heap_composition: HeapComposition,
    /// B2: dominator-depth histogram (retention shape). depth = idom-hops to the
    /// virtual root. Sorted by depth ascending. Always present; empty only for
    /// an empty heap. Surfaced in OOM Triage as a synthesized "Shape" line; the
    /// full histogram lives in JSON. Excludes the synthetic system-classloader
    /// object (it has no graph node).
    pub dominator_depth_histogram: Vec<DepthBucket>,
    /// B3: retention concentration over top-level dominators. Always present
    /// (zeroed for an empty heap). Surfaced in OOM Triage as a "One leak or
    /// many" line.
    pub retention_concentration: RetentionSummary,
    pub classes_loaded: u64,
    /// Count of DISTINCT class-loader addresses among loaded classes (boot
    /// loader counted once when present). This is "loaders referenced by loaded
    /// classes", NOT MAT's loader-object count — not a parity-gated scalar.
    pub classloaders_loaded: u64,
    pub unreachable_count: u64,
    pub unreachable_shallow: u64,
    pub histogram: Vec<HistRow>,
    /// Number of histogram rows the full histogram was capped to, or None when
    /// the histogram is complete (never truncated). Always None today.
    pub histogram_truncated_to: Option<u64>,
    /// Decoded JVM system properties (java.lang.System static `props`), sorted
    /// by key. Empty when the props object is absent or its layout could not be
    /// safely decoded (graceful fallback — never garbage). Additive: not
    /// parity-compared.
    pub system_properties: Vec<PropEntry>,
    /// Derived JVM version (prefers `java.vm.version`, else `java.version`).
    /// None when neither property was decoded. Additive: not parity-compared.
    pub jvm_version: Option<String>,
    /// F2: per-loader rollup over the histogram, top-N by retained heap.
    /// Additive bounded reduction (<= #loaders rows). Not parity-compared.
    pub loader_rollup: Vec<LoaderRollup>,
    /// F2: class names loaded under more than one loader (duplicate-class /
    /// class-loader-leak signature), capped. Additive. Not parity-compared.
    pub duplicate_classes: Vec<DuplicateClass>,
}

/// One step of a single-suspect accumulation path.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct PathStep {
    pub depth: usize,
    pub obj_index_1based: usize,
    pub display_class: String,
    pub retained: u64,
}

/// One immediately-dominated child of an accumulation point (a row of the
/// "Accumulated Objects in Dominator Tree" list).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DominatedRow {
    pub obj_index_1based: usize,
    pub display_class: String,
    pub shallow: u64,
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
    /// Descent from the suspect object to the accumulation point (MAT
    /// `findAccumulationPoint`, big-drop-ratio 0.7). Non-empty only for singles.
    pub path: Vec<PathStep>,
    /// The accumulation point object where retained heap piles up (the last
    /// step of `path`). `None` for group suspects or when the descent hit
    /// `MAX_ACCUM_DEPTH` without a big drop.
    pub accumulation_obj_1based: Option<usize>,
    pub accumulation_class: Option<String>,
    pub accumulation_retained: Option<u64>,
    /// Top immediately-dominated children of the accumulation point, sorted
    /// retained-desc and capped at the configured cap. Empty for groups.
    pub dominated: Vec<DominatedRow>,
    /// F3: FULL count of immediately-dominated children of the accumulation
    /// point (the dominator-children CSR degree, uncapped). The number the
    /// capped `dominated` list cannot convey — "how many objects does this
    /// accumulation point directly hold?". 0 for group suspects / no accum.
    pub dominated_total_count: u64,
    /// F3: how many rows the `dominated` list actually shows (== dominated.len()),
    /// so a renderer can say "showing top M of N".
    pub dominated_shown: u64,
    /// By-class histogram (objects/shallow/retained) of the accumulation
    /// point's immediately-dominated children, sorted retained-desc and
    /// capped. Empty for groups.
    pub dominated_by_class: Vec<HistRow>,
    /// Class names involved in this suspect (suspect class + accumulation
    /// point class), de-duplicated in first-seen order, for search.
    pub keywords: Vec<String>,
    /// Human label of the GC-root TYPE holding this suspect (e.g. "Thread",
    /// "Sticky Class", "JNI Global"), when the suspect's top-level dominator is
    /// itself an identifiable single GC root. Empty when unknown: the suspect
    /// is not itself a root, is held by multiple/ambiguous roots, or the root
    /// type is `ROOT_UNKNOWN`. Only populated for single suspects.
    pub root_type_label: String,
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

/// A single thread's call stack, resolved from HPROF STACK_TRACE/STACK_FRAME
/// records. Identifies the thread by its heap object (index + class) since the
/// thread NAME requires decoding java.lang.Thread fields (a later step).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ThreadInfo {
    /// HPROF thread serial (stable within the dump).
    pub thread_serial: u32,
    /// Decoded `java.lang.Thread.name`, or None when the name could not be
    /// resolved (missing thread/String, JDK layout mismatch, or empty name).
    /// Additive field: not part of MAT parity comparison.
    pub name: Option<String>,
    /// Class name of the resolved thread object, or None when the thread
    /// object could not be located in the heap.
    pub class_name: Option<String>,
    /// Stack frames, top-first, each "class.method (source:line)".
    pub frames: Vec<String>,
    /// Number of GC-thread-local roots this thread holds that resolve to a live
    /// object (from the dominator graph's synthetic thread→local edges). A high
    /// count flags a thread pinning many objects alive. Additive field: not part
    /// of MAT parity comparison. Bounded (per-thread), off the per-object budget.
    pub local_root_count: u64,
}

/// Aggregates for the "Threads" section: one entry per resolved stack trace.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ThreadOverview {
    /// Threads with call stacks, sorted by thread serial for determinism.
    pub threads: Vec<ThreadInfo>,
}

/// Schema version for the machine-readable JSON output. Bump on any
/// breaking change to the `Report` shape; the JSON always carries this.
pub const SCHEMA_VERSION: u32 = 4;

/// Full report data model: only bounded aggregates, never a per-object Vec.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Report {
    pub schema_version: u32,
    pub generated: String,
    pub overview: SystemOverview,
    pub leaks: LeakSuspects,
    pub top: TopConsumers,
    pub threads: ThreadOverview,
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
pub fn build_model(
    g: &Graph,
    dc_offsets: &[u32],
    dc_targets: &[u32],
    leak_children_cap: usize,
    depth_counts: &[u64],
) -> Report {
    let generated = now_iso8601();
    crate::trace::probe("build_model: before system_overview aggregates");
    let overview = build_system_overview(g, depth_counts);
    crate::trace::probe("build_model: after system_overview aggregates");
    let leaks = build_leak_suspects(g, dc_offsets, dc_targets, leak_children_cap);
    crate::trace::probe("build_model: after leak_suspects aggregates");
    let top = build_top_consumers(g);
    crate::trace::probe("build_model: after top_consumers aggregates");
    let threads = build_thread_overview(g);
    crate::trace::probe("build_model: after thread_overview aggregates");
    Report {
        schema_version: SCHEMA_VERSION,
        generated,
        overview,
        leaks,
        top,
        threads,
    }
}

/// Resolve each thread stack into a `ThreadInfo`. The thread's class name is
/// looked up via its object index (`u32::MAX` = unresolved). Small: one entry
/// per stack trace.
fn build_thread_overview(g: &Graph) -> ThreadOverview {
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
            ThreadInfo {
                thread_serial: t.thread_serial,
                name: g.thread_names.get(&t.thread_serial).cloned(),
                class_name,
                frames: t.frames.clone(),
                local_root_count: g
                    .thread_local_counts
                    .get(&t.thread_serial)
                    .copied()
                    .unwrap_or(0),
            }
        })
        .collect();
    ThreadOverview { threads }
}

fn build_system_overview(g: &Graph, depth_counts: &[u64]) -> SystemOverview {
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

    // F2: class-loader rollup + duplicate-class detection. Both are bounded
    // folds over `histogram` (one pass; maps keyed by loader_id / pretty_class,
    // so at most #loaders / #class-names entries — no per-object arrays).
    let (loader_rollup, duplicate_classes) = {
        use std::collections::HashMap;
        // Rollup: aggregate per loader_id.
        let mut roll: HashMap<u64, LoaderRollup> = HashMap::new();
        // Duplicate detection: per pretty_class, the distinct loader ids and
        // (labels, totals) seen. Labels de-duped in first-seen order.
        struct DupAcc {
            loader_ids: std::collections::HashSet<u64>,
            loaders: Vec<String>,
            total_instances: u64,
            total_retained: u64,
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
                });
            if d.loader_ids.insert(row.loader_id) {
                const LOADER_CAP: usize = 8;
                if d.loaders.len() < LOADER_CAP {
                    d.loaders.push(
                        row.loader_label
                            .clone()
                            .unwrap_or_else(|| format!("loader@{:#x}", row.loader_id)),
                    );
                }
            }
            d.total_instances += row.instances;
            d.total_retained += row.retained;
        }

        let mut rollup: Vec<LoaderRollup> = roll.into_values().collect();
        rollup.sort_unstable_by(|a, b| {
            b.retained
                .cmp(&a.retained)
                .then(a.loader_id.cmp(&b.loader_id))
        });
        rollup.truncate(TOP_N);

        let mut dups: Vec<DuplicateClass> = dup
            .into_iter()
            .filter(|(_, d)| d.loader_ids.len() > 1)
            .map(|(pretty_class, d)| DuplicateClass {
                pretty_class,
                loader_count: d.loader_ids.len() as u64,
                loaders: d.loaders,
                total_instances: d.total_instances,
                total_retained: d.total_retained,
            })
            .collect();
        dups.sort_unstable_by(|a, b| {
            b.total_retained
                .cmp(&a.total_retained)
                .then_with(|| a.pretty_class.cmp(&b.pretty_class))
        });
        dups.truncate(TOP_N);
        (rollup, dups)
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
    }
}

fn build_leak_suspects(
    g: &Graph,
    dc_offsets: &[u32],
    dc_targets: &[u32],
    cap: usize,
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

    // Materialise into the model, resolving the accumulation point for singles
    // via MAT's findAccumulationPoint (big-drop-ratio descent) and the holding
    // GC-root type.
    let out: Vec<Suspect> = suspects
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
    render_executive_summary(r, &mut out);
    render_oom_triage(r, &mut out);
    render_system_overview(&r.overview, &mut out);
    render_leak_suspects(&r.leaks, &mut out);
    render_top_consumers(&r.top, r.leaks.total_shallow, &mut out);
    render_threads(&r.threads, &mut out);
    out
}

/// `md-graphs` output: the same Markdown report enriched with in-text ASCII/Unicode
/// graphics (a linked table of contents, proportional bar columns, tree-drawn
/// packages, and a sparkline depth histogram). Rendered independently of plain
/// `md` so the byte-exact `md` output is never perturbed.
pub fn render_markdown_graphs(r: &Report) -> String {
    let mut out = String::new();
    render_title(&r.overview, &r.generated, &mut out);
    render_toc_graphs(&mut out);
    render_executive_summary(r, &mut out);
    render_oom_triage(r, &mut out);
    render_system_overview_graphs(&r.overview, &mut out);
    render_leak_suspects_graphs(&r.leaks, &mut out);
    render_top_consumers_graphs(&r.top, r.leaks.total_shallow, &mut out);
    render_threads(&r.threads, &mut out);
    out
}

/// Linked in-document table of contents for the graphics report. The anchors
/// use GitHub's slug convention (lowercase, spaces → hyphens) matching the
/// `##`/`###` headings emitted by the section renderers.
fn render_toc_graphs(out: &mut String) {
    out.push_str("## Contents\n\n");
    out.push_str("- [Summary](#summary)\n");
    out.push_str("- [OOM Triage](#oom-triage)\n");
    out.push_str("- [System Overview](#system-overview)\n");
    out.push_str("- [Leak Suspects](#leak-suspects)\n");
    out.push_str("- [Top Consumers](#top-consumers)\n");
    out.push_str("- [Threads](#threads)\n");
    out.push('\n');
    out.push_str("----\n\n");
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

/// Executive summary: a scannable digest at the very top of the report, before
/// the detailed sections. Two compact mini-tables (a handful of rows each)
/// re-project data already in the model — the headline scalars from System
/// Overview and the top few retainers by retained heap — so a reader gets an
/// at-a-glance answer to "what caused the OOM / where is the heap concentrated?"
/// without scrolling. The full detail tables follow unchanged below. Pure
/// function of `Report` (no new model fields, no graph access).
fn render_executive_summary(r: &Report, out: &mut String) {
    use crate::md::{Align, Table};
    /// Rows shown in the top-suspects digest; the full lists follow below.
    const SUMMARY_SUSPECTS: usize = 5;

    out.push_str("## Summary\n\n");
    out.push_str("_At-a-glance digest; see the sections below for full detail._\n\n");

    // Key stats: the headline scalars the System Overview already exposes.
    let o = &r.overview;
    let mut stats = Table::new(&["Metric", "Value"], &[Align::Left, Align::Right]);
    stats.row([
        "Total heap (reachable)".into(),
        format_bytes(o.total_shallow),
    ]);
    stats.row(["Objects".into(), fmt_count(o.total_objects)]);
    stats.row(["Classes".into(), fmt_count(o.classes_loaded)]);
    stats.row(["Class loaders".into(), fmt_count(o.classloaders_loaded)]);
    stats.row(["Threads".into(), fmt_count(r.threads.threads.len() as u64)]);
    stats.row(["GC roots".into(), fmt_count(o.gc_roots)]);
    stats.render(out);
    out.push('\n');

    // Top suspects / biggest retained: the single most important OOM signal,
    // shown up front. Prefer the leak-suspects list; fall back to the biggest
    // top-level objects when no suspect exceeds the threshold. Percentage basis
    // matches the detail tables: retained / total reachable shallow heap.
    let total = r.leaks.total_shallow;
    let pct_of = |retained: u64| -> f64 {
        if total > 0 {
            retained as f64 / total as f64 * 100.0
        } else {
            0.0
        }
    };

    if !r.leaks.suspects.is_empty() {
        out.push_str("**Top suspects by retained heap**\n\n");
        let mut t = Table::new(
            &["#", "Suspect", "Retained", "% Heap"],
            &[Align::Right, Align::Left, Align::Right, Align::Right],
        );
        for (rank, s) in r.leaks.suspects.iter().take(SUMMARY_SUSPECTS).enumerate() {
            let what = if s.is_single {
                format!("`{}` (single object)", s.pretty_class)
            } else {
                format!(
                    "`{}` ({} instances)",
                    s.pretty_class,
                    fmt_count(s.instance_count)
                )
            };
            t.row([
                (rank + 1).to_string(),
                what,
                format_bytes(s.retained),
                format!("{:.1}%", pct_of(s.retained)),
            ]);
        }
        t.render(out);
    } else if !r.top.biggest_objects.is_empty() {
        out.push_str("**Biggest retained objects**\n\n");
        let mut t = Table::new(
            &["#", "Class", "Retained", "% Heap"],
            &[Align::Right, Align::Left, Align::Right, Align::Right],
        );
        for (rank, ob) in r
            .top
            .biggest_objects
            .iter()
            .take(SUMMARY_SUSPECTS)
            .enumerate()
        {
            t.row([
                (rank + 1).to_string(),
                format!("`{}` (object #{})", ob.display_class, ob.obj_index_1based),
                format_bytes(ob.retained),
                format!("{:.1}%", pct_of(ob.retained)),
            ]);
        }
        t.render(out);
    } else {
        out.push_str("_No dominant retainer found._\n");
    }
    out.push('\n');
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

    // Shape (B2): shallow vs. deep retention, from the dominator-depth histogram.
    let hist = &r.overview.dominator_depth_histogram;
    if !hist.is_empty() {
        let total: u64 = hist.iter().map(|b| b.objects).sum();
        let max_depth = hist.iter().map(|b| b.depth).max().unwrap_or(0);
        // p90 depth: smallest depth whose cumulative count reaches 90%.
        let mut cum = 0u64;
        let mut p90 = max_depth;
        for b in hist {
            cum += b.objects;
            if cum * 10 >= total * 9 {
                p90 = b.depth;
                break;
            }
        }
        let shape = if p90 <= 3 {
            "shallow (most objects are held within a few hops of a GC root)"
        } else {
            "deep (retention flows through long dominator chains — often nested collections or linked structures)"
        };
        out.push_str(&format!(
            "- **Shape:** {shape} — 90% of objects within depth {p90}, max depth {max_depth}.\n"
        ));
    }

    // One leak or many (B3): from the retention-concentration summary.
    let rc = &r.overview.retention_concentration;
    if rc.top1_bp > 0 || rc.num_objects_ge_1pct > 0 {
        let top1_pct = rc.top1_bp as f64 / 100.0;
        let top10_pct = rc.top10_bp as f64 / 100.0;
        out.push_str(&format!(
            "- **One leak or many:** the single biggest object retains {:.1}% and the top 10 retain {:.1}% of the heap; {} object(s) each hold >=1%.\n",
            top1_pct, top10_pct, rc.num_objects_ge_1pct,
        ));
    }
    out.push('\n');
}

fn render_system_overview(o: &SystemOverview, out: &mut String) {
    use crate::md::{Align, Table};
    out.push_str("## System Overview\n\n");
    out.push_str("_Reachable-heap totals and the largest classes by retained heap._\n\n");
    out.push_str("### Heap Summary\n\n");
    let mut summary = Table::new(&["Property", "Value"], &[Align::Left, Align::Left]);
    summary.row(["HPROF format".into(), o.format.clone()]);
    summary.row(["File size".into(), format_bytes(o.file_size)]);
    summary.row([
        "Identifier size".into(),
        format!("{}-bit", o.identifier_size_bits),
    ]);
    if let Some(coops) = o.compressed_oops {
        summary.row([
            "Compressed OOPs".into(),
            if coops { "yes" } else { "no" }.into(),
        ]);
    }
    if let Some(ms) = o.dump_creation {
        summary.row(["Dump created".into(), format_epoch_ms(ms)]);
    }
    if let Some(ver) = &o.jvm_version {
        summary.row(["JVM version".into(), ver.clone()]);
    }
    summary.row(["Total objects".into(), fmt_count(o.total_objects)]);
    summary.row(["Total shallow heap".into(), format_bytes(o.total_shallow)]);
    summary.row(["GC roots".into(), fmt_count(o.gc_roots)]);
    summary.row(["Classes loaded".into(), fmt_count(o.classes_loaded)]);
    summary.row(["Class loaders".into(), fmt_count(o.classloaders_loaded)]);
    if o.unreachable_count > 0 {
        summary.row([
            "Unreachable objects (excluded)".into(),
            format!(
                "{} ({})",
                fmt_count(o.unreachable_count),
                format_bytes(o.unreachable_shallow),
            ),
        ]);
    }
    summary.render(out);
    out.push('\n');

    // Class-loader labels (additive; does not restructure the tables above).
    // List the distinct non-boot loader labels seen across histogram rows, in
    // first-seen order, capped for readability. Skips the `<boot>` label.
    {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut labels: Vec<&str> = Vec::new();
        for row in &o.histogram {
            if let Some(lbl) = row.loader_label.as_deref() {
                if lbl != "<boot>" && seen.insert(lbl) {
                    labels.push(lbl);
                }
            }
        }
        if !labels.is_empty() {
            const CAP: usize = 8;
            let shown = labels.len().min(CAP);
            let mut line = labels[..shown].join(", ");
            if labels.len() > CAP {
                line.push_str(&format!(", … (+{} more)", labels.len() - CAP));
            }
            out.push_str(&format!("- **Class loaders (labels):** {line}\n\n"));
        }
    }

    // System properties (additive; captured from java.lang.System.props). Table
    // capped for readability; the full sorted list lives in JSON. Values are
    // truncated to keep rows scannable.
    if !o.system_properties.is_empty() {
        const CAP: usize = 40;
        const VAL_MAX: usize = 120;
        out.push_str("### System Properties\n\n");
        let shown = o.system_properties.len().min(CAP);
        let mut t = Table::new(&["Property", "Value"], &[Align::Left, Align::Left]);
        for p in &o.system_properties[..shown] {
            let mut v = p.value.replace('\n', " ").replace('|', "\\|");
            if v.chars().count() > VAL_MAX {
                let truncated: String = v.chars().take(VAL_MAX).collect();
                v = format!("{truncated}…");
            }
            t.row([p.key.clone(), v]);
        }
        t.render(out);
        if o.system_properties.len() > CAP {
            out.push_str(&format!(
                "\n_… (+{} more properties in JSON)_\n",
                o.system_properties.len() - CAP
            ));
        }
        out.push('\n');
    }

    // (a single-type breakdown restates the "GC roots" scalar above).
    if o.gc_roots_by_type.len() > 1 {
        out.push_str("### GC Roots by Type\n\n");
        let mut t = Table::new(&["Root Type", "Count"], &[Align::Left, Align::Right]);
        for row in &o.gc_roots_by_type {
            t.row([row.root_type.clone(), fmt_count(row.count)]);
        }
        t.render(out);
        out.push('\n');
    }

    // Heap composition by kind: worth a table only when >1 kind present
    // (a single-kind heap just restates "Total objects").
    if o.heap_composition.by_kind.len() > 1 {
        out.push_str("### Heap Composition\n\n");
        let mut t = Table::new(
            &["Kind", "Objects", "Shallow Heap"],
            &[Align::Left, Align::Right, Align::Right],
        );
        for k in &o.heap_composition.by_kind {
            t.row([
                k.kind.clone(),
                fmt_count(k.objects),
                format_bytes(k.shallow_heap),
            ]);
        }
        t.render(out);
        out.push('\n');
    }

    // Retention Concentration (B3): how much of the heap the few biggest
    // top-level dominators hold. Basis points → percent (100 bp = 1%).
    {
        let rc = &o.retention_concentration;
        if rc.top1_bp > 0 || rc.top10_bp > 0 || rc.top100_bp > 0 || rc.num_objects_ge_1pct > 0 {
            out.push_str("### Retention Concentration\n\n");
            out.push_str(
                "_Share of reachable heap held by the largest top-level dominators; \
                 a high top-1 share points to a single dominant leak._\n\n",
            );
            let mut t = Table::new(&["Scope", "Retained Share"], &[Align::Left, Align::Right]);
            t.row([
                "Top 1 object".into(),
                format!("{:.1}%", rc.top1_bp as f64 / 100.0),
            ]);
            t.row([
                "Top 10 objects".into(),
                format!("{:.1}%", rc.top10_bp as f64 / 100.0),
            ]);
            t.row([
                "Top 100 objects".into(),
                format!("{:.1}%", rc.top100_bp as f64 / 100.0),
            ]);
            t.row([
                "Objects each >=1%".into(),
                fmt_count(rc.num_objects_ge_1pct),
            ]);
            t.render(out);
            out.push('\n');
        }
    }

    // Dominator-Depth Distribution (B2): objects per idom-hop below a GC root.
    if !o.dominator_depth_histogram.is_empty() {
        const DEPTH_CAP: usize = 50;
        out.push_str("### Dominator-Depth Distribution\n\n");
        out.push_str(
            "_Objects per hop-count below a GC root; a tall shallow side means shallow retention._\n\n",
        );
        let total = o.dominator_depth_histogram.len();
        let shown = total.min(DEPTH_CAP);
        let mut t = Table::new(&["Depth", "Objects"], &[Align::Right, Align::Right]);
        for b in o.dominator_depth_histogram.iter().take(shown) {
            t.row([b.depth.to_string(), fmt_count(b.objects)]);
        }
        t.render(out);
        if total > shown {
            out.push_str(&format!(
                "\n_… (+{} deeper buckets in JSON)_\n",
                total - shown
            ));
        }
        out.push('\n');
    }

    out.push_str("### Class Histogram (by Retained Heap)\n\n");
    out.push_str(
        "_Top 50 classes ranked by retained heap; the full list is in the JSON output._\n\n",
    );
    let mut hist = Table::new(
        &["#", "Class", "Instances", "Shallow Heap", "Retained Heap"],
        &[
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
        ],
    );
    // The model carries the FULL histogram; the Markdown view shows the top 50
    // rows for readability. The complete data lives in the JSON output.
    // Retained heap uses human-readable byte units (matching every other
    // retained/shallow column) so the scale is scannable at a glance.
    for (rank, row) in o.histogram.iter().take(50).enumerate() {
        hist.row([
            (rank + 1).to_string(),
            format!("`{}`", row.pretty_class),
            fmt_count(row.instances),
            format_bytes(row.shallow),
            format_bytes(row.retained),
        ]);
    }
    hist.render(out);
    out.push('\n');

    // Class Loaders (F2): per-loader rollup, top-N by retained heap.
    if !o.loader_rollup.is_empty() {
        out.push_str("### Class Loaders\n\n");
        out.push_str(
            "_Classes grouped by the loader that defined them; many loaders each holding heap \
             can signal a class-loader leak._\n\n",
        );
        let mut t = Table::new(
            &[
                "Loader",
                "Classes",
                "Instances",
                "Shallow Heap",
                "Retained Heap",
            ],
            &[
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Right,
            ],
        );
        for r in &o.loader_rollup {
            t.row([
                r.loader_label.clone().unwrap_or_else(|| "<unknown>".into()),
                fmt_count(r.class_count),
                fmt_count(r.instances),
                format_bytes(r.shallow),
                format_bytes(r.retained),
            ]);
        }
        t.render(out);
        out.push('\n');
    }

    // Duplicate Classes (F2): class names loaded under more than one loader.
    if !o.duplicate_classes.is_empty() {
        out.push_str("### Duplicate Classes\n\n");
        out.push_str(
            "_Class names loaded by more than one class loader — a classic class-loader-leak \
             signature (the same class re-loaded repeatedly)._\n\n",
        );
        let mut t = Table::new(
            &["Class", "#Loaders", "Instances", "Retained Heap"],
            &[Align::Left, Align::Right, Align::Right, Align::Right],
        );
        for d in &o.duplicate_classes {
            t.row([
                format!("`{}`", d.pretty_class),
                fmt_count(d.loader_count),
                fmt_count(d.total_instances),
                format_bytes(d.total_retained),
            ]);
        }
        t.render(out);
        out.push('\n');
    }
}

fn render_leak_suspects(l: &LeakSuspects, out: &mut String) {
    out.push_str("## Leak Suspects\n\n");

    if l.suspects.is_empty() {
        out.push_str("No single object or class group exceeds the threshold.\n\n");
        return;
    }

    out.push_str(
        "_Objects and class groups whose retained heap is large enough to be a likely OOM cause, ranked by retained heap._\n\n",
    );

    for (rank, s) in l.suspects.iter().enumerate() {
        let pct = if l.total_shallow > 0 {
            s.retained as f64 / l.total_shallow as f64 * 100.0
        } else {
            0.0
        };

        out.push_str(&format!(
            "### {}. `{}` — retains {} ({:.1}% of reachable heap)\n\n",
            rank + 1,
            s.pretty_class,
            format_bytes(s.retained),
            pct,
        ));

        // What the suspect is: a single object vs a class group.
        if s.is_single {
            out.push_str(&format!(
                "One `{}` object (shallow {}) dominates this retained heap.\n\n",
                s.pretty_class,
                format_bytes(s.shallow),
            ));
        } else {
            out.push_str(&format!(
                "{} instances of `{}` together retain this heap (combined shallow {}).\n\n",
                fmt_count(s.instance_count),
                s.pretty_class,
                format_bytes(s.shallow),
            ));
        }

        // Accumulation point: where the retained heap actually piles up.
        if s.is_single {
            if !s.root_type_label.is_empty() {
                out.push_str(&format!("Held by a **{}** GC root.\n\n", s.root_type_label));
            }
            match (
                &s.accumulation_class,
                s.accumulation_obj_1based,
                s.accumulation_retained,
            ) {
                (Some(ac), Some(obj), Some(ret)) => {
                    if s.path.len() <= 1 {
                        out.push_str(&format!(
                            "This object is itself the accumulation point (retained {}).\n\n",
                            format_bytes(ret),
                        ));
                    } else {
                        out.push_str(&format!(
                            "Retained heap accumulates at `{}` (object #{}, retained {}).\n\n",
                            ac,
                            obj,
                            format_bytes(ret),
                        ));
                    }
                }
                _ => {
                    out.push_str(
                        "No single accumulation point was found within the search depth.\n\n",
                    );
                }
            }
        }

        // Accumulated objects (immediately dominated by the accumulation point).
        if !s.dominated.is_empty() {
            use crate::md::{Align, Table};
            if s.dominated_total_count > s.dominated_shown {
                out.push_str(&format!(
                    "_Directly dominates {} objects (showing top {})._\n\n",
                    fmt_count(s.dominated_total_count),
                    fmt_count(s.dominated_shown),
                ));
            } else if s.dominated_total_count > 0 {
                out.push_str(&format!(
                    "_Directly dominates {} objects._\n\n",
                    fmt_count(s.dominated_total_count),
                ));
            }
            out.push_str(&format!(
                "**Accumulated objects (top {} by retained heap):**\n\n",
                s.dominated.len(),
            ));
            let mut t = Table::new(
                &["Object Index", "Class", "Shallow", "Retained"],
                &[Align::Right, Align::Left, Align::Right, Align::Right],
            );
            for row in &s.dominated {
                t.row([
                    row.obj_index_1based.to_string(),
                    format!("`{}`", row.display_class),
                    format_bytes(row.shallow),
                    format_bytes(row.retained),
                ]);
            }
            t.render(out);
            out.push('\n');
        }

        // By-class histogram of the accumulated objects.
        if !s.dominated_by_class.is_empty() {
            use crate::md::{Align, Table};
            out.push_str("**Accumulated objects by class:**\n\n");
            let mut t = Table::new(
                &["Class", "Objects", "Shallow", "Retained"],
                &[Align::Left, Align::Right, Align::Right, Align::Right],
            );
            for row in &s.dominated_by_class {
                t.row([
                    format!("`{}`", row.pretty_class),
                    fmt_count(row.instances),
                    format_bytes(row.shallow),
                    format_bytes(row.retained),
                ]);
            }
            t.render(out);
            out.push('\n');
        }
    }
}

fn render_top_consumers(t: &TopConsumers, total_shallow: u64, out: &mut String) {
    use crate::md::{Align, Table};
    out.push_str("## Top Consumers\n\n");
    out.push_str("### Biggest Objects (Top-Level Dominators)\n\n");
    out.push_str(
        "_Individual objects retaining the most heap; `% Heap` is the share of total reachable heap._\n\n",
    );
    let mut objs = Table::new(
        &[
            "#",
            "Object Index",
            "Class",
            "Shallow",
            "Retained",
            "% Heap",
        ],
        &[
            Align::Right,
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
        ],
    );
    for (rank, row) in t.biggest_objects.iter().enumerate() {
        let pct = if total_shallow > 0 {
            row.retained as f64 / total_shallow as f64 * 100.0
        } else {
            0.0
        };
        objs.row([
            (rank + 1).to_string(),
            row.obj_index_1based.to_string(),
            format!("`{}`", row.display_class),
            format_bytes(row.shallow),
            format_bytes(row.retained),
            format!("{:.1}%", pct),
        ]);
    }
    objs.render(out);
    out.push('\n');

    out.push_str("### Biggest Classes by Retained Heap\n\n");
    out.push_str("_Classes whose instances together retain the most heap._\n\n");
    let mut classes = Table::new(
        &["#", "Class", "Instances", "Retained Heap"],
        &[Align::Right, Align::Left, Align::Right, Align::Right],
    );
    for (rank, row) in t.biggest_classes.iter().enumerate() {
        classes.row([
            (rank + 1).to_string(),
            format!("`{}`", row.pretty_class),
            fmt_count(row.instances),
            format_bytes(row.retained),
        ]);
    }
    classes.render(out);
    out.push('\n');

    out.push_str("### Biggest Packages by Retained Heap\n\n");
    if t.biggest_packages.children.is_empty() {
        out.push_str("_No package retains more than 1% of the total retained heap._\n");
        out.push('\n');
        return;
    }
    out.push_str(
        "_Retained heap aggregated by package prefix (rows retaining <1% of the total are pruned)._\n\n",
    );
    let mut pkgs = Table::new(
        &["Package", "Objects", "Shallow", "Retained"],
        &[Align::Left, Align::Right, Align::Right, Align::Right],
    );
    // Pre-order DFS; the displayed name is the full dotted path accumulated
    // down from the root, so each row is self-describing (no tree-drawing chars).
    fn emit_node(node: &PackageNode, prefix: &str, pkgs: &mut Table) {
        let full = if prefix.is_empty() {
            node.name.clone()
        } else {
            format!("{}.{}", prefix, node.name)
        };
        pkgs.row([
            format!("`{}`", full),
            fmt_count(node.top_dominator_count),
            format_bytes(node.shallow_heap),
            format_bytes(node.retained_heap),
        ]);
        for child in &node.children {
            emit_node(child, &full, pkgs);
        }
    }
    // Skip the synthetic root (name ""); start emitting at its children.
    for child in &t.biggest_packages.children {
        emit_node(child, "", &mut pkgs);
    }
    pkgs.render(out);
    out.push('\n');
}

// ── md-graphs section renderers ─────────────────────────────────────────────
// These mirror the plain-Markdown sections byte-for-byte in their data, but add
// proportional bar columns, a sparkline, and tree-drawn package hierarchy. They
// are only reachable from `render_markdown_graphs`; plain `md` never calls them.

/// Width (in cells) of the in-table proportional bar columns. Fixed so columns
/// stay aligned regardless of the values.
const GRAPH_BAR_WIDTH: usize = 16;

/// System Overview with bar columns on GC Roots / Heap Composition, a sparkline
/// for the dominator-depth distribution, and a share bar on the class histogram.
fn render_system_overview_graphs(o: &SystemOverview, out: &mut String) {
    use crate::md::{Align, Table, bar, sparkline};
    out.push_str("## System Overview\n\n");
    out.push_str("_Reachable-heap totals and the largest classes by retained heap._\n\n");
    out.push_str("### Heap Summary\n\n");
    let mut summary = Table::new(&["Property", "Value"], &[Align::Left, Align::Left]);
    summary.row(["HPROF format".into(), o.format.clone()]);
    summary.row(["File size".into(), format_bytes(o.file_size)]);
    summary.row([
        "Identifier size".into(),
        format!("{}-bit", o.identifier_size_bits),
    ]);
    if let Some(coops) = o.compressed_oops {
        summary.row([
            "Compressed OOPs".into(),
            if coops { "yes" } else { "no" }.into(),
        ]);
    }
    if let Some(ms) = o.dump_creation {
        summary.row(["Dump created".into(), format_epoch_ms(ms)]);
    }
    if let Some(ver) = &o.jvm_version {
        summary.row(["JVM version".into(), ver.clone()]);
    }
    summary.row(["Total objects".into(), fmt_count(o.total_objects)]);
    summary.row(["Total shallow heap".into(), format_bytes(o.total_shallow)]);
    summary.row(["GC roots".into(), fmt_count(o.gc_roots)]);
    summary.row(["Classes loaded".into(), fmt_count(o.classes_loaded)]);
    summary.row(["Class loaders".into(), fmt_count(o.classloaders_loaded)]);
    if o.unreachable_count > 0 {
        summary.row([
            "Unreachable objects (excluded)".into(),
            format!(
                "{} ({})",
                fmt_count(o.unreachable_count),
                format_bytes(o.unreachable_shallow),
            ),
        ]);
    }
    summary.render(out);
    out.push('\n');

    {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut labels: Vec<&str> = Vec::new();
        for row in &o.histogram {
            if let Some(lbl) = row.loader_label.as_deref() {
                if lbl != "<boot>" && seen.insert(lbl) {
                    labels.push(lbl);
                }
            }
        }
        if !labels.is_empty() {
            const CAP: usize = 8;
            let shown = labels.len().min(CAP);
            let mut line = labels[..shown].join(", ");
            if labels.len() > CAP {
                line.push_str(&format!(", … (+{} more)", labels.len() - CAP));
            }
            out.push_str(&format!("- **Class loaders (labels):** {line}\n\n"));
        }
    }

    if !o.system_properties.is_empty() {
        const CAP: usize = 40;
        const VAL_MAX: usize = 120;
        out.push_str("### System Properties\n\n");
        let shown = o.system_properties.len().min(CAP);
        let mut t = Table::new(&["Property", "Value"], &[Align::Left, Align::Left]);
        for p in &o.system_properties[..shown] {
            let mut v = p.value.replace('\n', " ").replace('|', "\\|");
            if v.chars().count() > VAL_MAX {
                let truncated: String = v.chars().take(VAL_MAX).collect();
                v = format!("{truncated}…");
            }
            t.row([p.key.clone(), v]);
        }
        t.render(out);
        if o.system_properties.len() > CAP {
            out.push_str(&format!(
                "\n_… (+{} more properties in JSON)_\n",
                o.system_properties.len() - CAP
            ));
        }
        out.push('\n');
    }

    // GC Roots by Type — with a proportional count bar.
    if o.gc_roots_by_type.len() > 1 {
        out.push_str("### GC Roots by Type\n\n");
        let max = o
            .gc_roots_by_type
            .iter()
            .map(|r| r.count)
            .max()
            .unwrap_or(0);
        let mut t = Table::new(
            &["Root Type", "Count", ""],
            &[Align::Left, Align::Right, Align::Left],
        );
        for row in &o.gc_roots_by_type {
            t.row([
                row.root_type.clone(),
                fmt_count(row.count),
                bar(row.count, max, GRAPH_BAR_WIDTH),
            ]);
        }
        t.render(out);
        out.push('\n');
    }

    // Heap Composition — with a proportional shallow-heap bar.
    if o.heap_composition.by_kind.len() > 1 {
        out.push_str("### Heap Composition\n\n");
        let max = o
            .heap_composition
            .by_kind
            .iter()
            .map(|k| k.shallow_heap)
            .max()
            .unwrap_or(0);
        let mut t = Table::new(
            &["Kind", "Objects", "Shallow Heap", ""],
            &[Align::Left, Align::Right, Align::Right, Align::Left],
        );
        for k in &o.heap_composition.by_kind {
            t.row([
                k.kind.clone(),
                fmt_count(k.objects),
                format_bytes(k.shallow_heap),
                bar(k.shallow_heap, max, GRAPH_BAR_WIDTH),
            ]);
        }
        t.render(out);
        out.push('\n');
    }

    // Retention Concentration (B3) — same numbers as plain md (data parity).
    {
        let rc = &o.retention_concentration;
        if rc.top1_bp > 0 || rc.top10_bp > 0 || rc.top100_bp > 0 || rc.num_objects_ge_1pct > 0 {
            out.push_str("### Retention Concentration\n\n");
            out.push_str(
                "_Share of reachable heap held by the largest top-level dominators; \
                 a high top-1 share points to a single dominant leak._\n\n",
            );
            let mut t = Table::new(
                &["Scope", "Retained Share", ""],
                &[Align::Left, Align::Right, Align::Left],
            );
            t.row([
                "Top 1 object".into(),
                format!("{:.1}%", rc.top1_bp as f64 / 100.0),
                bar(rc.top1_bp as u64, 10_000, GRAPH_BAR_WIDTH),
            ]);
            t.row([
                "Top 10 objects".into(),
                format!("{:.1}%", rc.top10_bp as f64 / 100.0),
                bar(rc.top10_bp as u64, 10_000, GRAPH_BAR_WIDTH),
            ]);
            t.row([
                "Top 100 objects".into(),
                format!("{:.1}%", rc.top100_bp as f64 / 100.0),
                bar(rc.top100_bp as u64, 10_000, GRAPH_BAR_WIDTH),
            ]);
            t.row([
                "Objects each >=1%".into(),
                fmt_count(rc.num_objects_ge_1pct),
                String::new(),
            ]);
            t.render(out);
            out.push('\n');
        }
    }

    // Dominator-Depth Distribution — a sparkline over the per-depth object
    // counts, PLUS the full per-depth table (data parity with plain md).
    if !o.dominator_depth_histogram.is_empty() {
        out.push_str("### Dominator-Depth Distribution\n\n");
        out.push_str(
            "_Objects per hop-count below a GC root; a tall left side means shallow retention._\n\n",
        );
        let counts: Vec<u64> = o
            .dominator_depth_histogram
            .iter()
            .map(|b| b.objects)
            .collect();
        let first = o
            .dominator_depth_histogram
            .first()
            .map(|b| b.depth)
            .unwrap_or(0);
        let last = o
            .dominator_depth_histogram
            .last()
            .map(|b| b.depth)
            .unwrap_or(0);
        out.push_str(&format!(
            "`{}`  (depth {}–{})\n\n",
            sparkline(&counts),
            first,
            last,
        ));
        const DEPTH_CAP: usize = 50;
        let dmax = counts.iter().copied().max().unwrap_or(0);
        let total = o.dominator_depth_histogram.len();
        let shown = total.min(DEPTH_CAP);
        let mut t = Table::new(
            &["Depth", "Objects", ""],
            &[Align::Right, Align::Right, Align::Left],
        );
        for b in o.dominator_depth_histogram.iter().take(shown) {
            t.row([
                b.depth.to_string(),
                fmt_count(b.objects),
                bar(b.objects, dmax, GRAPH_BAR_WIDTH),
            ]);
        }
        t.render(out);
        if total > shown {
            out.push_str(&format!(
                "\n_… (+{} deeper buckets in JSON)_\n",
                total - shown
            ));
        }
        out.push('\n');
    }

    out.push_str("### Class Histogram (by Retained Heap)\n\n");
    out.push_str(
        "_Top 50 classes ranked by retained heap; the full list is in the JSON output._\n\n",
    );
    let hist_max = o
        .histogram
        .iter()
        .take(50)
        .map(|r| r.retained)
        .max()
        .unwrap_or(0);
    let mut hist = Table::new(
        &[
            "#",
            "Class",
            "Instances",
            "Shallow Heap",
            "Retained Heap",
            "",
        ],
        &[
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Left,
        ],
    );
    for (rank, row) in o.histogram.iter().take(50).enumerate() {
        hist.row([
            (rank + 1).to_string(),
            format!("`{}`", row.pretty_class),
            fmt_count(row.instances),
            format_bytes(row.shallow),
            format_bytes(row.retained),
            bar(row.retained, hist_max, GRAPH_BAR_WIDTH),
        ]);
    }
    hist.render(out);
    out.push('\n');

    // Class Loaders (F2) — with a proportional retained-heap bar.
    if !o.loader_rollup.is_empty() {
        out.push_str("### Class Loaders\n\n");
        out.push_str(
            "_Classes grouped by the loader that defined them; many loaders each holding heap \
             can signal a class-loader leak._\n\n",
        );
        let lmax = o
            .loader_rollup
            .iter()
            .map(|r| r.retained)
            .max()
            .unwrap_or(0);
        let mut t = Table::new(
            &[
                "Loader",
                "Classes",
                "Instances",
                "Shallow Heap",
                "Retained Heap",
                "",
            ],
            &[
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Left,
            ],
        );
        for r in &o.loader_rollup {
            t.row([
                r.loader_label.clone().unwrap_or_else(|| "<unknown>".into()),
                fmt_count(r.class_count),
                fmt_count(r.instances),
                format_bytes(r.shallow),
                format_bytes(r.retained),
                bar(r.retained, lmax, GRAPH_BAR_WIDTH),
            ]);
        }
        t.render(out);
        out.push('\n');
    }

    // Duplicate Classes (F2) — same table as plain md (no extra glyph column;
    // #Loaders is already the salient number).
    if !o.duplicate_classes.is_empty() {
        out.push_str("### Duplicate Classes\n\n");
        out.push_str(
            "_Class names loaded by more than one class loader — a classic class-loader-leak \
             signature (the same class re-loaded repeatedly)._\n\n",
        );
        let mut t = Table::new(
            &["Class", "#Loaders", "Instances", "Retained Heap"],
            &[Align::Left, Align::Right, Align::Right, Align::Right],
        );
        for d in &o.duplicate_classes {
            t.row([
                format!("`{}`", d.pretty_class),
                fmt_count(d.loader_count),
                fmt_count(d.total_instances),
                format_bytes(d.total_retained),
            ]);
        }
        t.render(out);
        out.push('\n');
    }
}

/// Leak Suspects with a leading share-bar table across all suspects, then the
/// full plain per-suspect detail (reused verbatim for byte-identical numbers).
fn render_leak_suspects_graphs(l: &LeakSuspects, out: &mut String) {
    use crate::md::{Align, Table, bar};
    out.push_str("## Leak Suspects\n\n");

    if l.suspects.is_empty() {
        out.push_str("No single object or class group exceeds the threshold.\n\n");
        return;
    }

    out.push_str(
        "_Objects and class groups whose retained heap is large enough to be a likely OOM cause, ranked by retained heap._\n\n",
    );

    // Share overview: one proportional bar per suspect, keyed to the largest
    // suspect's retained heap so the relative sizes read at a glance.
    let max = l.suspects.iter().map(|s| s.retained).max().unwrap_or(0);
    let mut share = Table::new(
        &["#", "Suspect", "Retained", "% Heap", ""],
        &[
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Left,
        ],
    );
    for (rank, s) in l.suspects.iter().enumerate() {
        let pct = if l.total_shallow > 0 {
            s.retained as f64 / l.total_shallow as f64 * 100.0
        } else {
            0.0
        };
        share.row([
            (rank + 1).to_string(),
            format!("`{}`", s.pretty_class),
            format_bytes(s.retained),
            format!("{pct:.1}%"),
            bar(s.retained, max, GRAPH_BAR_WIDTH),
        ]);
    }
    share.render(out);
    out.push('\n');

    // Per-suspect detail: identical to plain Markdown.
    for (rank, s) in l.suspects.iter().enumerate() {
        let pct = if l.total_shallow > 0 {
            s.retained as f64 / l.total_shallow as f64 * 100.0
        } else {
            0.0
        };

        out.push_str(&format!(
            "### {}. `{}` — retains {} ({:.1}% of reachable heap)\n\n",
            rank + 1,
            s.pretty_class,
            format_bytes(s.retained),
            pct,
        ));

        if s.is_single {
            out.push_str(&format!(
                "One `{}` object (shallow {}) dominates this retained heap.\n\n",
                s.pretty_class,
                format_bytes(s.shallow),
            ));
        } else {
            out.push_str(&format!(
                "{} instances of `{}` together retain this heap (combined shallow {}).\n\n",
                fmt_count(s.instance_count),
                s.pretty_class,
                format_bytes(s.shallow),
            ));
        }

        if s.is_single {
            if !s.root_type_label.is_empty() {
                out.push_str(&format!("Held by a **{}** GC root.\n\n", s.root_type_label));
            }
            match (
                &s.accumulation_class,
                s.accumulation_obj_1based,
                s.accumulation_retained,
            ) {
                (Some(ac), Some(obj), Some(ret)) => {
                    if s.path.len() <= 1 {
                        out.push_str(&format!(
                            "This object is itself the accumulation point (retained {}).\n\n",
                            format_bytes(ret),
                        ));
                    } else {
                        out.push_str(&format!(
                            "Retained heap accumulates at `{}` (object #{}, retained {}).\n\n",
                            ac,
                            obj,
                            format_bytes(ret),
                        ));
                    }
                }
                _ => {
                    out.push_str(
                        "No single accumulation point was found within the search depth.\n\n",
                    );
                }
            }
        }

        if !s.dominated.is_empty() {
            if s.dominated_total_count > s.dominated_shown {
                out.push_str(&format!(
                    "_Directly dominates {} objects (showing top {})._\n\n",
                    fmt_count(s.dominated_total_count),
                    fmt_count(s.dominated_shown),
                ));
            } else if s.dominated_total_count > 0 {
                out.push_str(&format!(
                    "_Directly dominates {} objects._\n\n",
                    fmt_count(s.dominated_total_count),
                ));
            }
            out.push_str(&format!(
                "**Accumulated objects (top {} by retained heap):**\n\n",
                s.dominated.len(),
            ));
            let mut t = Table::new(
                &["Object Index", "Class", "Shallow", "Retained"],
                &[Align::Right, Align::Left, Align::Right, Align::Right],
            );
            for row in &s.dominated {
                t.row([
                    row.obj_index_1based.to_string(),
                    format!("`{}`", row.display_class),
                    format_bytes(row.shallow),
                    format_bytes(row.retained),
                ]);
            }
            t.render(out);
            out.push('\n');
        }

        if !s.dominated_by_class.is_empty() {
            out.push_str("**Accumulated objects by class:**\n\n");
            let mut t = Table::new(
                &["Class", "Objects", "Shallow", "Retained"],
                &[Align::Left, Align::Right, Align::Right, Align::Right],
            );
            for row in &s.dominated_by_class {
                t.row([
                    format!("`{}`", row.pretty_class),
                    fmt_count(row.instances),
                    format_bytes(row.shallow),
                    format_bytes(row.retained),
                ]);
            }
            t.render(out);
            out.push('\n');
        }
    }
}

/// Top Consumers with share bars on Biggest Objects / Classes and a tree-drawn
/// package hierarchy (box-drawing connectors + a retained-heap bar per row).
fn render_top_consumers_graphs(t: &TopConsumers, total_shallow: u64, out: &mut String) {
    use crate::md::{Align, Table, bar, tree_prefix};
    out.push_str("## Top Consumers\n\n");
    out.push_str("### Biggest Objects (Top-Level Dominators)\n\n");
    out.push_str(
        "_Individual objects retaining the most heap; `% Heap` is the share of total reachable heap._\n\n",
    );
    let obj_max = t
        .biggest_objects
        .iter()
        .map(|r| r.retained)
        .max()
        .unwrap_or(0);
    let mut objs = Table::new(
        &[
            "#",
            "Object Index",
            "Class",
            "Shallow",
            "Retained",
            "% Heap",
            "",
        ],
        &[
            Align::Right,
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Left,
        ],
    );
    for (rank, row) in t.biggest_objects.iter().enumerate() {
        let pct = if total_shallow > 0 {
            row.retained as f64 / total_shallow as f64 * 100.0
        } else {
            0.0
        };
        objs.row([
            (rank + 1).to_string(),
            row.obj_index_1based.to_string(),
            format!("`{}`", row.display_class),
            format_bytes(row.shallow),
            format_bytes(row.retained),
            format!("{pct:.1}%"),
            bar(row.retained, obj_max, GRAPH_BAR_WIDTH),
        ]);
    }
    objs.render(out);
    out.push('\n');

    out.push_str("### Biggest Classes by Retained Heap\n\n");
    out.push_str("_Classes whose instances together retain the most heap._\n\n");
    let cls_max = t
        .biggest_classes
        .iter()
        .map(|r| r.retained)
        .max()
        .unwrap_or(0);
    let mut classes = Table::new(
        &["#", "Class", "Instances", "Retained Heap", ""],
        &[
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Left,
        ],
    );
    for (rank, row) in t.biggest_classes.iter().enumerate() {
        classes.row([
            (rank + 1).to_string(),
            format!("`{}`", row.pretty_class),
            fmt_count(row.instances),
            format_bytes(row.retained),
            bar(row.retained, cls_max, GRAPH_BAR_WIDTH),
        ]);
    }
    classes.render(out);
    out.push('\n');

    out.push_str("### Biggest Packages by Retained Heap\n\n");
    if t.biggest_packages.children.is_empty() {
        out.push_str("_No package retains more than 1% of the total retained heap._\n");
        out.push('\n');
        return;
    }
    out.push_str(
        "_Retained heap aggregated by package prefix (rows retaining <1% of the total are pruned); the tree shows nesting._\n\n",
    );
    // Bar is keyed to the largest top-level package's retained heap.
    let pkg_max = t
        .biggest_packages
        .children
        .iter()
        .map(|c| c.retained_heap)
        .max()
        .unwrap_or(0);
    let mut pkgs = Table::new(
        &["Package", "Objects", "Shallow", "Retained", ""],
        &[
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Left,
        ],
    );
    // Pre-order DFS with box-drawing prefixes. Each row shows only this node's
    // own segment name (last dotted component), indented by its tree position;
    // the full path is implied by the nesting rather than repeated.
    fn emit_node_tree(
        node: &PackageNode,
        depth: usize,
        is_last: bool,
        ancestors_continue: &[bool],
        pkg_max: u64,
        pkgs: &mut Table,
    ) {
        let prefix = tree_prefix(depth, is_last, ancestors_continue);
        // Show the leaf segment (final dotted component) for depth > 0; the full
        // name at the top level so top rows stay self-describing.
        let label = if depth == 0 {
            node.name.clone()
        } else {
            node.name
                .rsplit('.')
                .next()
                .unwrap_or(&node.name)
                .to_string()
        };
        pkgs.row([
            format!("{prefix}`{label}`"),
            fmt_count(node.top_dominator_count),
            format_bytes(node.shallow_heap),
            format_bytes(node.retained_heap),
            bar(node.retained_heap, pkg_max, GRAPH_BAR_WIDTH),
        ]);
        let n = node.children.len();
        for (i, child) in node.children.iter().enumerate() {
            let child_last = i + 1 == n;
            let mut cont = ancestors_continue.to_vec();
            cont.push(!is_last);
            emit_node_tree(child, depth + 1, child_last, &cont, pkg_max, pkgs);
        }
    }
    let n = t.biggest_packages.children.len();
    for (i, child) in t.biggest_packages.children.iter().enumerate() {
        emit_node_tree(child, 0, i + 1 == n, &[], pkg_max, &mut pkgs);
    }
    pkgs.render(out);
    out.push('\n');
}

/// Render the "Threads" section: each resolved thread's call stack. Threads
/// without any frames are already dropped upstream; an empty section prints a
/// placeholder so the heading is still self-describing.
fn render_threads(t: &ThreadOverview, out: &mut String) {
    out.push_str("## Threads\n\n");
    if t.threads.is_empty() {
        out.push_str("_No thread call stacks were recorded in this dump._\n\n");
        return;
    }
    for th in &t.threads {
        let class = th.class_name.as_deref().unwrap_or("<unresolved>");
        match &th.name {
            Some(name) => out.push_str(&format!(
                "### Thread {} \"{}\" ({})\n\n",
                th.thread_serial, name, class
            )),
            None => out.push_str(&format!("### Thread {} ({})\n\n", th.thread_serial, class)),
        }
        if th.local_root_count > 0 {
            out.push_str(&format!(
                "_Local roots: {}._\n\n",
                fmt_count(th.local_root_count)
            ));
        }
        for frame in &th.frames {
            out.push_str(&format!("- `{frame}`\n"));
        }
        out.push('\n');
    }
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::md_test::Md;
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
            class_loader_id: vec![0u64; class_names.len()],
            loader_labels: std::collections::HashMap::new(),
            thread_stacks: Vec::new(),
            thread_names: std::collections::HashMap::new(),
            thread_local_counts: std::collections::HashMap::new(),
            system_properties: Vec::new(),
            jvm_version: None,
            class_obj_class_idx,
            fwd_offsets: Vec::new(),
            fwd_targets: Vec::new(),
            synthetic_root_count,
            system_classloader_shallow: None,
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

    /// Test-only wrapper: derives the B2 `depth_counts` histogram from `g.idom`
    /// (the way the old per-object memo scan did) and calls the real
    /// `build_model`. Production tallies `depth_counts` for free inside
    /// `compute_retained`'s dominator-tree DFS; test graphs are tiny so
    /// recomputing here is irrelevant.
    fn build_model_t(g: &Graph, dc_off: &[u32], dc_tgt: &[u32], cap: usize) -> Report {
        let n = g.n;
        let vroot = n as u32;
        let undef = u32::MAX;
        let mut depth_counts: Vec<u64> = Vec::new();
        for u in 0..n {
            // A node is reachable iff it has a defined idom (roots have idom
            // = vroot). Walk up to vroot counting hops; depth 1 = under vroot.
            let mut cur = u as u32;
            if g.idom[u] == undef {
                continue;
            }
            let mut depth = 0usize;
            while cur != vroot {
                let p = g.idom[cur as usize];
                if p == undef {
                    depth = 0;
                    break;
                }
                depth += 1;
                cur = p;
            }
            if depth == 0 {
                continue;
            }
            if depth > depth_counts.len() {
                depth_counts.resize(depth, 0);
            }
            depth_counts[depth - 1] += 1;
        }
        build_model(g, dc_off, dc_tgt, cap, &depth_counts)
    }

    #[test]
    fn test_build_model_system_overview() {
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
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

    /// GC-roots-by-type breakdown: counts each reachable root by its HPROF
    /// sub-tag label, subtracts the synthetic System-Class roots (so the rows
    /// sum to the reported `gc_roots` scalar), and sorts count-desc / label-asc.
    #[test]
    fn test_gc_roots_by_type_breakdown() {
        use crate::types::heap;
        let (mut g, dc_off, dc_tgt) = fixture();
        // fixture() has 3 reachable roots (obj0, obj1, obj3). Give them types:
        // two System Class (one of which is synthetic) + one Thread. With
        // synthetic_root_count = 1, the synthetic System Class root is removed,
        // leaving System Class = 1 and Thread = 1.
        g.gc_root_types = vec![
            heap::ROOT_SYSTEM_CLASS,
            heap::ROOT_SYSTEM_CLASS,
            heap::ROOT_THREAD_OBJ,
        ];
        g.synthetic_root_count = 1;

        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let o = &r.overview;
        // Scalar: 3 roots - 1 synthetic = 2.
        assert_eq!(o.gc_roots, 2);
        // Rows must sum to the scalar.
        let sum: u64 = o.gc_roots_by_type.iter().map(|r| r.count).sum();
        assert_eq!(sum, o.gc_roots);
        // Sorted count-desc, then label-asc: both have count 1, so "System
        // Class" (S) precedes "Thread" (T) alphabetically.
        assert_eq!(o.gc_roots_by_type.len(), 2);
        assert_eq!(o.gc_roots_by_type[0].root_type, "System Class");
        assert_eq!(o.gc_roots_by_type[0].count, 1);
        assert_eq!(o.gc_roots_by_type[1].root_type, "Thread");
        assert_eq!(o.gc_roots_by_type[1].count, 1);
    }

    /// When every synthetic root fills a label bucket exactly, that bucket must
    /// be dropped (not left at count 0).
    #[test]
    fn test_gc_roots_by_type_drops_emptied_bucket() {
        use crate::types::heap;
        let (mut g, dc_off, dc_tgt) = fixture();
        // 3 roots: 1 System Class (synthetic) + 2 JNI Global. Removing the 1
        // synthetic System Class empties that bucket entirely.
        g.gc_root_types = vec![
            heap::ROOT_SYSTEM_CLASS,
            heap::ROOT_JNI_GLOBAL,
            heap::ROOT_JNI_GLOBAL,
        ];
        g.synthetic_root_count = 1;

        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let o = &r.overview;
        assert_eq!(o.gc_roots, 2);
        assert_eq!(o.gc_roots_by_type.len(), 1);
        assert_eq!(o.gc_roots_by_type[0].root_type, "JNI Global");
        assert_eq!(o.gc_roots_by_type[0].count, 2);
    }

    // ── B5: heap composition by kind ────────────────────────────────────────

    #[test]
    fn test_object_kind_derivation() {
        // A graph with one of each kind: an instance, an object array, a
        // primitive array, and a class object (present in class_obj_class_idx).
        // idom: all top-level under vroot=4.
        let (g, _dc_off, _dc_tgt) = make_graph(
            vec![4, 4, 4, 4], // idom (vroot = 4)
            vec![0, 1, 2, 3], // class_idx
            vec![16, 24, 32, 8],
            vec![16, 24, 32, 8],
            vec!["com/foo/A", "[Ljava/lang/Object;", "[I", "java/lang/Class"],
            &[(3, 0)], // obj3 is a class object representing class0
            &[],
            vec![],
            0,
        );
        assert_eq!(object_kind(&g, 0), "Instances");
        assert_eq!(object_kind(&g, 1), "Object arrays");
        assert_eq!(object_kind(&g, 2), "Primitive arrays");
        assert_eq!(object_kind(&g, 3), "Class objects");
    }

    #[test]
    fn test_heap_composition_fixed_order_skips_empty() {
        // Two instances + one primitive array; NO object arrays, NO class
        // objects. by_kind must list Instances then Primitive arrays only,
        // preserving the fixed kind order and skipping empty buckets.
        let (g, dc_off, dc_tgt) = make_graph(
            vec![3, 3, 3], // idom (vroot = 3)
            vec![0, 0, 1], // class_idx
            vec![16, 16, 40],
            vec![16, 16, 40],
            vec!["com/foo/A", "[I"],
            &[],
            &[],
            vec![],
            0,
        );
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let bk = &r.overview.heap_composition.by_kind;
        assert_eq!(bk.len(), 2);
        assert_eq!(bk[0].kind, "Instances");
        assert_eq!(bk[0].objects, 2);
        assert_eq!(bk[0].shallow_heap, 32);
        assert_eq!(bk[1].kind, "Primitive arrays");
        assert_eq!(bk[1].objects, 1);
        assert_eq!(bk[1].shallow_heap, 40);
    }

    // ── B2: dominator-depth histogram ───────────────────────────────────────

    #[test]
    fn test_dominator_depth_histogram() {
        // fixture(): obj0/obj1/obj3 are top-level (depth 1); obj2 is dominated
        // by obj0 (depth 2); obj4 is unreachable (excluded).
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let h = &r.overview.dominator_depth_histogram;
        assert_eq!(h.len(), 2);
        // Sorted by depth ascending.
        assert_eq!(h[0].depth, 1);
        assert_eq!(h[0].objects, 3);
        assert_eq!(h[1].depth, 2);
        assert_eq!(h[1].objects, 1);
    }

    // ── B3: retention concentration ─────────────────────────────────────────

    #[test]
    fn test_retention_concentration() {
        // fixture(): top-level dominators retained = [1000, 1000, 200];
        // total_shallow = 270 (denominator). one_pct = 270/100 = 2, so all
        // three top-level objects (>=2) count toward num_objects_ge_1pct.
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let rc = &r.overview.retention_concentration;
        assert_eq!(rc.total_retained, 2200);
        // top1 = 1000/270 = 37037 bp; top10 (all 3) = 2200/270 = 81481 bp.
        assert_eq!(rc.top1_bp, (1000u128 * 10_000 / 270) as u32);
        assert_eq!(rc.top10_bp, (2200u128 * 10_000 / 270) as u32);
        assert_eq!(rc.top100_bp, rc.top10_bp);
        assert_eq!(rc.num_objects_ge_1pct, 3);
    }

    // ── OOM Triage render lines (B2/B3/B5 surfaced) ─────────────────────────

    #[test]
    fn test_render_includes_oom_triage_signals() {
        // Mixed-kind graph so the Heap Composition table renders (>1 kind), with
        // top-level dominators so Shape + One-leak-or-many lines emit.
        // obj0 instance (top-level), obj1 primitive array (top-level),
        // obj2 instance dominated by obj0.
        let (g, dc_off, dc_tgt) = make_graph(
            vec![3, 3, 0], // idom (vroot = 3); obj2 under obj0
            vec![0, 1, 0], // class_idx
            vec![100, 40, 20],
            vec![120, 40, 20],
            vec!["com/foo/A", "[I"],
            &[],
            &[2], // obj2 has same-class ancestor (obj0)
            vec![0, 1],
            0,
        );
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let md = render_markdown(&r);
        assert!(
            md.contains("### Heap Composition"),
            "missing heap composition table"
        );
        assert!(md.contains("**Shape:**"), "missing shape line");
        assert!(
            md.contains("**One leak or many:**"),
            "missing concentration line"
        );
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
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
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

    /// Task 19: class-loader identity flows Graph -> report. Three classes, two
    /// distinct loaders (0 = boot, 0x1000). Two of the three are reachable class
    /// objects (mapped via class_obj_class_idx). `classloaders_loaded` counts
    /// distinct loaders among reachable class objects; each HistRow carries the
    /// loader of its class; the Markdown renders a "Class loaders" line.
    #[test]
    fn test_class_loader_plumbing() {
        // Rows: 0 = com/foo/A (loader 0x1000), 1 = com/foo/B (loader 0x1000),
        //       2 = org/bar/C (loader 0 = boot).
        // Objects: obj0 IS a class object -> row 0; obj1 IS a class object ->
        // row 2; obj2 is a plain instance of row 1. vroot = 3.
        let (mut g, _dc_off, _dc_tgt) = make_graph(
            vec![3, 3, 3],        // idom (vroot = 3)
            vec![0, 2, 1],        // class_idx
            vec![100, 50, 20],    // shallow
            vec![1000, 500, 200], // retained
            vec!["com/foo/A", "com/foo/B", "org/bar/C"],
            &[(0, 0), (1, 2)], // obj0 -> row 0, obj1 -> row 2 (class objects)
            &[],
            vec![0, 1, 2],
            0,
        );
        // Assign loaders per histogram row: rows 0,1 = 0x1000; row 2 = boot(0).
        g.class_loader_id = vec![0x1000, 0x1000, 0];
        let (dc_off, dc_tgt) = crate::retained::build_dom_children_csr(g.n, &g.idom);
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let o = &r.overview;

        // Reachable class objects: obj0 (row 0, loader 0x1000) and obj1 (row 2,
        // loader 0). Two distinct loaders.
        assert_eq!(o.classes_loaded, 2);
        assert_eq!(o.classloaders_loaded, 2);

        // Each HistRow carries its class's loader.
        let a = o
            .histogram
            .iter()
            .find(|h| h.pretty_class == "com.foo.A")
            .expect("com.foo.A row");
        assert_eq!(a.loader_id, 0x1000);
        let c = o
            .histogram
            .iter()
            .find(|h| h.pretty_class == "org.bar.C")
            .expect("org.bar.C row");
        assert_eq!(c.loader_id, 0);

        // Markdown surfaces the Class loaders line.
        let md = render_markdown(&r);
        assert!(md.contains("Class loaders"), "missing Class loaders line");
    }

    /// A boot-only heap (all loaders 0) reports exactly one class loader.
    #[test]
    fn test_class_loader_boot_only() {
        let (mut g, _dc_off, _dc_tgt) = make_graph(
            vec![2, 2],
            vec![0, 1],
            vec![100, 50],
            vec![1000, 500],
            vec!["java/lang/Class", "com/foo/A"],
            &[(0, 1)], // obj0 is a class object representing row 1
            &[],
            vec![0, 1],
            0,
        );
        g.class_loader_id = vec![0, 0];
        let (dc_off, dc_tgt) = crate::retained::build_dom_children_csr(g.n, &g.idom);
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        assert_eq!(r.overview.classloaders_loaded, 1);
    }

    /// Stage 1: `HistRow.loader_label` resolves the boot loader (addr 0) to
    /// `<boot>` and a named loader address to its label from `loader_labels`.
    /// The Markdown "Class loaders (labels)" line lists the non-boot label.
    #[test]
    fn test_loader_label_resolution() {
        // Row 0 = boot-loaded (addr 0); row 1 = loaded by 0x1234.
        let (mut g, _dc_off, _dc_tgt) = make_graph(
            vec![2, 2],
            vec![0, 1],
            vec![100, 50],
            vec![1000, 500],
            vec!["java/lang/Class", "com/foo/A"],
            &[(0, 0), (1, 1)],
            &[],
            vec![0, 1],
            0,
        );
        g.class_loader_id = vec![0, 0x1234];
        g.loader_labels
            .insert(0x1234, "com/example/MyLoader".to_string());
        let (dc_off, dc_tgt) = crate::retained::build_dom_children_csr(g.n, &g.idom);
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let o = &r.overview;

        let boot = o
            .histogram
            .iter()
            .find(|h| h.loader_id == 0)
            .expect("boot-loaded row present");
        assert_eq!(boot.loader_label.as_deref(), Some("<boot>"));

        let named = o
            .histogram
            .iter()
            .find(|h| h.loader_id == 0x1234)
            .expect("0x1234-loaded row present");
        assert_eq!(named.loader_label.as_deref(), Some("com/example/MyLoader"));

        // Markdown surfaces the label (not the boot pseudo-label) in the list.
        let md = render_markdown(&r);
        assert!(
            md.contains("**Class loaders (labels):** com/example/MyLoader"),
            "missing Class loaders labels line; got:\n{md}"
        );
    }

    /// MAT materializes a synthetic <system class loader> object at 0x0 of
    /// class java/lang/ClassLoader (no HPROF record). When
    /// `system_classloader_shallow` is set, the report injects one such object:
    /// +1 total_objects, +sz total_shallow, +1 instance / +sz shallow on the
    /// java.lang.ClassLoader histogram row. With `None`, everything is
    /// unchanged (regression guard). gc_roots/classes_loaded stay untouched.
    #[test]
    fn test_synthetic_system_classloader_injection() {
        // obj0: java/lang/ClassLoader instance, top-level, shallow 72.
        // obj1: com/foo/A instance, top-level.
        let build = || {
            make_graph(
                vec![2, 2],   // idom (vroot = 2)
                vec![0, 1],   // class_idx
                vec![72, 40], // shallow
                vec![72, 40], // retained
                vec!["java/lang/ClassLoader", "com/foo/A"],
                &[], // no class objects
                &[], // none excluded
                vec![0, 1],
                0,
            )
        };

        // None path: nothing injected.
        {
            let (g, dc_off, dc_tgt) = build();
            assert_eq!(g.system_classloader_shallow, None);
            let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
            let o = &r.overview;
            assert_eq!(o.total_objects, 2);
            assert_eq!(o.total_shallow, 72 + 40);
            let cl_row = o
                .histogram
                .iter()
                .find(|h| h.pretty_class == "java.lang.ClassLoader")
                .expect("ClassLoader row present");
            assert_eq!(cl_row.instances, 1);
            assert_eq!(cl_row.shallow, 72);
        }

        // Some(72) path: one synthetic object injected.
        {
            let (mut g, dc_off, dc_tgt) = build();
            g.system_classloader_shallow = Some(72);
            let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
            let o = &r.overview;
            assert_eq!(o.total_objects, 3, "synthetic object not counted");
            assert_eq!(o.total_shallow, 72 + 40 + 72, "synthetic shallow missing");
            assert_eq!(o.gc_roots, 2, "gc_roots must be unchanged");
            assert_eq!(o.classes_loaded, 0, "classes_loaded must be unchanged");
            let cl_row = o
                .histogram
                .iter()
                .find(|h| h.pretty_class == "java.lang.ClassLoader")
                .expect("ClassLoader row present");
            assert_eq!(cl_row.instances, 2, "synthetic instance not in row");
            assert_eq!(cl_row.shallow, 72 + 72, "synthetic shallow not in row");
        }
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
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let o = &r.overview;
        assert_eq!(o.compressed_oops, Some(false)); // ref_size == id_size
        assert_eq!(o.dump_creation, None); // header_timestamp_ms == 0
    }

    #[test]
    fn test_build_model_top_consumers_package_determinism() {
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
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
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
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
        let mut r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let root = &r.top.biggest_packages;
        assert_eq!(root.top_dominator_count, count as u64);
        assert!(
            root.children.is_empty(),
            "no single package should exceed 1% of the total"
        );
        r.generated = "FIXED".to_string();
        let md = render_markdown(&r);
        let doc = Md::parse(&md);
        let pkgs = doc
            .section("Biggest Packages by Retained Heap")
            .expect("Biggest Packages section present");
        assert!(
            pkgs.body_contains("_No package retains more than 1% of the total retained heap._"),
            "nothing-over-threshold marker must be rendered under Biggest Packages"
        );
        // And the table must have no data rows in this case.
        assert!(
            pkgs.table(0).map(|t| t.rows().is_empty()).unwrap_or(true),
            "no package rows when nothing exceeds the threshold"
        );
    }

    #[test]
    fn test_build_model_leak_suspects() {
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
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
    fn test_accumulation_point_big_drop_and_leaf() {
        // Two top-level singles under vroot (node 6):
        //   A(obj0) -> B(obj1) -> {C(obj2), D(obj3)}   [big-drop chain]
        //   E(obj4) -> F(obj5)                          [leaf chain]
        // retained: A=1000 B=950 C=500 D=100 E=800 F=700.
        // A->B: 950 >= 1000*0.7=700 -> descend. B's largest child C=500 <
        //   950*0.7=665 -> BIG DROP -> accumulation point is B (the parent).
        // E->F: 700 >= 800*0.7=560 -> descend. F is a leaf -> accumulation is F.
        let (g, dc_off, dc_tgt) = make_graph(
            vec![6, 0, 1, 1, 6, 4],
            vec![0, 1, 2, 3, 4, 5],
            vec![10, 10, 10, 10, 10, 10],
            vec![1000, 950, 500, 100, 800, 700],
            vec!["A", "B", "C", "D", "E", "F"],
            &[],
            &[],
            vec![0, 4],
            0,
        );
        let l = build_leak_suspects(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        // Two singles: A (1000) then E (800), retained-desc.
        assert_eq!(l.suspects.len(), 2);
        let a = &l.suspects[0];
        assert_eq!(a.pretty_class, "A");
        // A descends to B and stops (big drop at C): path = [A, B].
        assert_eq!(a.path.len(), 2);
        assert_eq!(a.accumulation_obj_1based, Some(2)); // B is obj1 -> 1-based 2
        assert_eq!(a.accumulation_class, Some("B".to_string()));
        assert_eq!(a.accumulation_retained, Some(950));
        // B's immediately-dominated children, retained-desc: C(500), D(100).
        assert_eq!(a.dominated.len(), 2);
        assert_eq!(a.dominated[0].obj_index_1based, 3); // C = obj2
        assert_eq!(a.dominated[0].retained, 500);
        assert_eq!(a.dominated[1].obj_index_1based, 4); // D = obj3
        assert_eq!(a.dominated[1].retained, 100);
        // Keywords: suspect class + accumulation class.
        assert_eq!(a.keywords, vec!["A".to_string(), "B".to_string()]);

        // E chain: E -> F (leaf) -> accumulation point is F (obj5 -> 1-based 6).
        let e = &l.suspects[1];
        assert_eq!(e.pretty_class, "E");
        assert_eq!(e.accumulation_obj_1based, Some(6));
        assert_eq!(e.accumulation_class, Some("F".to_string()));
        // F is a leaf: no dominated children.
        assert!(e.dominated.is_empty());
    }

    #[test]
    fn test_accumulation_dominated_cap_truncates() {
        // A(obj0) is the accumulation point (its largest child drops below 0.7),
        // with 3 immediately-dominated children B,C,D.
        // retained: A=1000 B=100 C=90 D=80. 100 < 1000*0.7 -> A is accumulation.
        let (g, dc_off, dc_tgt) = make_graph(
            vec![4, 0, 0, 0],
            vec![0, 1, 2, 3],
            vec![10, 10, 10, 10],
            vec![1000, 100, 90, 80],
            vec!["A", "B", "C", "D"],
            &[],
            &[],
            vec![0],
            0,
        );
        // cap = 1 -> only the largest dominated child is listed.
        let l1 = build_leak_suspects(&g, &dc_off, &dc_tgt, 1);
        assert_eq!(l1.suspects.len(), 1);
        assert_eq!(l1.suspects[0].accumulation_obj_1based, Some(1)); // A itself
        assert_eq!(l1.suspects[0].dominated.len(), 1);
        assert_eq!(l1.suspects[0].dominated[0].obj_index_1based, 2); // B, largest
        // Default cap -> all three children listed.
        let l2 = build_leak_suspects(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        assert_eq!(l2.suspects[0].dominated.len(), 3);
    }

    #[test]
    fn test_leak_suspect_root_type_label() {
        // Fixture GC roots are objects 0, 1, 3 (all single suspects). Override
        // their root types: obj0 -> Thread, obj1 -> UNKNOWN (no label), obj3 ->
        // JNI Global. Suspects sort com.foo.A (obj0), com.foo.B (obj1),
        // org.bar.C (obj3).
        let (mut g, dc_off, dc_tgt) = fixture();
        use crate::types::heap;
        // gc_root_indices is [0, 1, 3]; align types 1:1.
        assert_eq!(g.gc_root_indices, vec![0, 1, 3]);
        g.gc_root_types = vec![
            heap::ROOT_THREAD_OBJ,
            heap::ROOT_UNKNOWN,
            heap::ROOT_JNI_GLOBAL,
        ];

        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let l = &r.leaks;
        // obj0 is a Thread root -> "Thread".
        assert_eq!(l.suspects[0].pretty_class, "com.foo.A");
        assert_eq!(l.suspects[0].root_type_label, "Thread");
        // obj1 is a root but ROOT_UNKNOWN -> no identifiable label (empty).
        assert_eq!(l.suspects[1].pretty_class, "com.foo.B");
        assert_eq!(l.suspects[1].root_type_label, "");
        // obj3 is a JNI Global root -> "JNI Global".
        assert_eq!(l.suspects[2].pretty_class, "org.bar.C");
        assert_eq!(l.suspects[2].root_type_label, "JNI Global");

        // The known labels render as the additive clause; the unknown one does not.
        let mut r2 = r.clone();
        r2.generated = "FIXED".to_string();
        let md = render_markdown(&r2);
        assert!(md.contains("Held by a **Thread** GC root."));
        assert!(md.contains("Held by a **JNI Global** GC root."));
    }

    #[test]
    fn test_leak_suspect_root_type_label_absent_when_not_root() {
        // A single suspect whose object is NOT a GC root gets no label. obj0 is
        // a top-level dominator (single suspect) but we make ONLY obj1 a root,
        // so obj0's suspect has an empty root_type_label.
        let (mut g, dc_off, dc_tgt) = fixture();
        use crate::types::heap;
        g.gc_root_indices = vec![1];
        g.gc_root_types = vec![heap::ROOT_THREAD_OBJ];

        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let l = &r.leaks;
        // obj0 (com.foo.A) is a single suspect but not itself a root -> empty.
        assert_eq!(l.suspects[0].pretty_class, "com.foo.A");
        assert!(l.suspects[0].is_single);
        assert_eq!(l.suspects[0].root_type_label, "");
        // obj1 (com.foo.B) is the Thread root -> labelled.
        assert_eq!(l.suspects[1].pretty_class, "com.foo.B");
        assert_eq!(l.suspects[1].root_type_label, "Thread");
    }

    #[test]
    fn test_leak_suspect_class_object_shows_represented_class() {
        // A single suspect whose object is itself a java.lang.Class MIRROR must
        // print the REPRESENTED class (e.g. scala.runtime.LazyVals$), not
        // "java.lang.Class" (MAT parity). Regression guard for report.rs:1127.
        //
        // 3 objects, 2 class rows:
        //   row0 = java/lang/Class, row1 = scala/runtime/LazyVals$
        //   obj0: class_idx row0 (a Class mirror), registered in
        //         class_obj_class_idx -> represents row1. Top-level, big retained.
        //   obj1: class_idx row1 (a normal instance), dominated by obj0.
        //   vroot = 2.
        let (g, dc_off, dc_tgt) = make_graph(
            vec![2, 0],        // idom: obj0 top-level, obj1 under obj0
            vec![0, 1],        // class_idx
            vec![24, 16],      // shallow
            vec![100_000, 16], // retained
            vec!["java/lang/Class", "scala/runtime/LazyVals$"],
            &[(0, 1)], // obj0 is a class-mirror representing row1
            &[],
            vec![0], // obj0 is a GC root
            0,
        );
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let s = &r.leaks.suspects[0];
        assert!(s.is_single);
        // The represented class, NOT "java.lang.Class".
        assert_eq!(s.pretty_class, "scala.runtime.LazyVals$");
        assert!(s.keywords.contains(&"scala.runtime.LazyVals$".to_string()));
        assert!(!s.keywords.contains(&"java.lang.Class".to_string()));

        let mut r2 = r.clone();
        r2.generated = "FIXED".to_string();
        let md = render_markdown(&r2);
        assert!(md.contains("scala.runtime.LazyVals$"));
    }

    #[test]
    fn test_render_markdown_deterministic() {
        // Build the model twice and assert render output is byte-identical.
        // This specifically guards the Biggest-Packages HashMap sort fix.
        let (g1, off1, tgt1) = fixture();
        let (g2, off2, tgt2) = fixture();
        let mut r1 = build_model_t(&g1, &off1, &tgt1, DOMINATED_CAP);
        let mut r2 = build_model_t(&g2, &off2, &tgt2, DOMINATED_CAP);
        // Neutralise the nondeterministic timestamp line.
        r1.generated = "FIXED".to_string();
        r2.generated = "FIXED".to_string();
        assert_eq!(render_markdown(&r1), render_markdown(&r2));
    }

    #[test]
    fn test_render_markdown_structure() {
        let (g, dc_off, dc_tgt) = fixture();
        let mut r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        r.generated = "FIXED".to_string();
        let md = render_markdown(&r);
        assert!(md.starts_with("# Heap Dump Analysis: `test.hprof`\n\n"));
        let doc = Md::parse(&md);
        // Top-level document title is an H1.
        assert_eq!(
            doc.heading("Heap Dump Analysis").map(|h| h.level()),
            Some(1)
        );
        // Major sections are H2.
        assert_eq!(doc.heading("System Overview").map(|h| h.level()), Some(2));
        assert_eq!(doc.heading("Leak Suspects").map(|h| h.level()), Some(2));
        assert_eq!(doc.heading("Top Consumers").map(|h| h.level()), Some(2));
        // Sub-sections are H3, nested under their parents.
        assert_eq!(
            doc.heading("Class Histogram (by Retained Heap)")
                .map(|h| h.level()),
            Some(3)
        );
        assert_eq!(
            doc.heading("Biggest Packages by Retained Heap")
                .map(|h| h.level()),
            Some(3)
        );
        // Class Histogram lives inside System Overview's body.
        assert!(
            doc.section("System Overview")
                .unwrap()
                .body_contains("### Class Histogram (by Retained Heap)")
        );
    }

    #[test]
    fn test_render_markdown_oom_triage() {
        let (g, dc_off, dc_tgt) = fixture();
        let mut r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        r.generated = "FIXED".to_string();
        let md = render_markdown(&r);
        let doc = Md::parse(&md);

        // (a) new OOM-triage heading + headline retainer line present.
        let triage = doc
            .section("OOM Triage")
            .expect("missing OOM Triage heading");
        assert_eq!(triage.level(), 2, "OOM Triage should be an H2 section");
        // The headline retainer is a bullet, not just loose text.
        assert!(
            triage.has_bullet_starting_with("**Headline retainer:**"),
            "missing headline retainer bullet"
        );
        // Fixture's #1 suspect is com.foo.A (a single object) at 1000/270 -> dominates.
        assert!(
            triage.has_bullet_containing("`com.foo.A`"),
            "headline should name the #1 suspect"
        );
        assert!(
            triage.has_bullet_containing("A single object/class group dominates the heap"),
            "1000/270 is >= 50% so it should read as dominated"
        );

        // The triage block must precede System Overview.
        let tri = doc.heading_offset("OOM Triage").unwrap();
        let sys = doc.heading_offset("System Overview").unwrap();
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
            assert!(doc.heading(needle).is_some(), "missing section: {needle}");
        }
    }

    // ── Phase B: JSON / schema conformance ─────────────────────────────────

    /// Build the fixture Report with the nondeterministic timestamp neutralised.
    fn fixture_report() -> Report {
        let (g, dc_off, dc_tgt) = fixture();
        let mut r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
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
            "schema/report.schema.json must equal a fresh schema_for!(Report);              regenerate via `dev emit-schema` if the model changed"
        );
    }

    #[test]
    fn schema_version_guard() {
        let r = fixture_report();
        assert_eq!(r.schema_version, SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 4);
    }

    #[test]
    fn thread_overview_resolves_class_from_object_index() {
        // Two objects: idx 0 is an instance of class row 1 ("java/lang/Thread"),
        // idx 1 is unreachable filler. A thread stack points at obj idx 0.
        let (mut g, _o, _t) = make_graph(
            vec![2, 2],
            vec![1, 0],
            vec![16, 16],
            vec![16, 16],
            vec!["Filler", "java/lang/Thread"],
            &[],
            &[],
            vec![],
            0,
        );
        g.thread_stacks = vec![
            crate::pass2::ThreadStack {
                thread_serial: 7,
                thread_obj_idx: 0,
                frames: vec!["java.lang.Object.wait (Object.java:1)".to_string()],
            },
            crate::pass2::ThreadStack {
                thread_serial: 9,
                thread_obj_idx: u32::MAX,
                frames: vec!["x.y (Unknown Source)".to_string()],
            },
        ];
        let ov = build_thread_overview(&g);
        assert_eq!(ov.threads.len(), 2);
        assert_eq!(ov.threads[0].thread_serial, 7);
        assert_eq!(
            ov.threads[0].class_name.as_deref(),
            Some("java/lang/Thread")
        );
        assert_eq!(ov.threads[0].frames.len(), 1);
        // Unresolved object index yields no class name.
        assert_eq!(ov.threads[1].class_name, None);
    }

    #[test]
    fn render_threads_emits_heading_and_frames() {
        let mut out = String::new();
        render_threads(
            &ThreadOverview {
                threads: vec![ThreadInfo {
                    thread_serial: 3,
                    name: Some("main".to_string()),
                    class_name: Some("java/lang/Thread".to_string()),
                    frames: vec!["java.lang.Object.wait (Object.java:1)".to_string()],
                    local_root_count: 0,
                }],
            },
            &mut out,
        );
        assert!(out.contains("## Threads"));
        assert!(out.contains("### Thread 3 \"main\" (java/lang/Thread)"));
        assert!(out.contains("java.lang.Object.wait (Object.java:1)"));
    }

    #[test]
    fn render_threads_handles_empty() {
        let mut out = String::new();
        render_threads(&ThreadOverview { threads: vec![] }, &mut out);
        assert!(out.contains("## Threads"));
        assert!(out.contains("No thread call stacks"));
    }
}
