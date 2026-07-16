//! The canonical report data model: pure-data structs and the schema
//! version, serialised to JSON via serde.

// ── Data model ──────────────────────────────────────────────────────────────

/// One row of the System-Overview class histogram (full, one row per class).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct HistRow {
    pub pretty_class: String,
    pub instances: u64,
    pub shallow: u64,
    pub retained: u64,
    /// Shallow size of the single largest instance of this class — surfaces a
    /// lone oversized object hiding behind a small instance count. `0` for
    /// synthetic rows that have no backing object (the injected system class
    /// loader, leak-suspect `dominated_by_class` rows). `#[serde(default)]` so
    /// pre-v5 reports (which lack the field) still deserialize.
    #[serde(default)]
    pub max_instance_shallow: u64,
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

/// One row of the unreachable-objects histogram: objects that are not
/// dominated by the virtual root (`idom == u32::MAX`), grouped by class.
/// Additive; not parity-compared. Sorted by shallow descending, capped.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct UnreachableClassRow {
    pub pretty_class: String,
    pub objects: u64,
    pub shallow: u64,
}

/// One row of the GC-roots-by-type breakdown: a human-readable root-type label
/// and how many roots carry that HPROF type.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GcRootTypeRow {
    pub root_type: String,
    pub count: u64,
}

/// One row of the GC-root-retained-by-type table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GcRootRetainedRow {
    pub root_type: String,
    pub count: u64,
    pub retained: u64,
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

/// One power-of-two size bucket of the top-level-dominator retained-size
/// distribution. `upper_bytes` is the inclusive upper bound (a power of two);
/// a dominator with retained size r falls in the smallest bucket whose
/// `upper_bytes >= r`. `count` is how many top-level dominators land here.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SizeBucket {
    pub upper_bytes: u64,
    pub count: u64,
}

/// Retained-size distribution over ALL top-level dominators (the biggest
/// memory contributors), bucketed by power-of-two retained size. Additive;
/// not parity-compared. `buckets` empty and stats zero when there are no
/// top-level dominators (empty heap).
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct TopSizeDistribution {
    pub buckets: Vec<SizeBucket>,
    pub count: u64,
    pub min: u64,
    pub max: u64,
    pub median: u64,
    pub total: u64,
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

/// Eclipse-MAT-style "Top Components": retained heap grouped by class loader
/// (component), with the top classes inside each. A bounded reduction over the
/// per-class retained aggregation (rows <= #loaders), so RSS-safe.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct TopComponents {
    /// Components sorted by retained desc, capped to the top N.
    pub components: Vec<Component>,
}

/// One component (class loader) in the Top Components view.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Component {
    /// Human-readable loader/component label (e.g. `<system class loader>`).
    pub loader_label: String,
    /// Total retained heap attributed to this component.
    pub retained: u64,
    /// Retained heap as a percentage of total reachable retained heap.
    pub pct: f64,
    /// Top classes within this component by retained heap (capped).
    pub top_classes: Vec<ComponentClass>,
}

/// One class row inside a component.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ComponentClass {
    pub pretty_class: String,
    pub retained: u64,
}

/// One class-loader's contribution to a duplicated class name (see
/// [`DuplicateClass`]). Additive; not parity-compared.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct DuplicateClassLoaderRow {
    /// Display label for this loader (e.g. `<boot>` or an app loader), or the
    /// synthesized `loader@0x…` fallback when no label was resolved.
    pub loader_label: String,
    pub loader_id: u64,
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
    /// Per-loader breakdown of this duplicated class (capped at the same
    /// LOADER_CAP as `loaders`), sorted by retained descending. Additive;
    /// `#[serde(default)]` so older JSON still deserializes.
    #[serde(default)]
    pub per_loader: Vec<DuplicateClassLoaderRow>,
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
    /// Ratio of unreachable shallow heap to total heap (reachable + unreachable).
    /// Range [0.0, 1.0]. 0.0 for an empty heap.
    #[serde(default)]
    pub heap_fragmentation_ratio: f64,
    /// Retained heap share of the single largest class, in integer basis points (100 bp = 1%).
    /// 0 for an empty heap.
    #[serde(default)]
    pub top_class_concentration_bp: u32,
    /// Retained heap grouped by GC root type. Additive; empty when no roots.
    #[serde(default)]
    pub gc_roots_retained_by_type: Vec<GcRootRetainedRow>,
    /// Per-class histogram of unreachable objects (idom == u32::MAX), sorted by
    /// shallow descending and capped. Additive; not parity-compared.
    #[serde(default)]
    pub unreachable_histogram: Vec<UnreachableClassRow>,
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
    /// HPROF record census: raw record-type counts for the dump (UTF8,
    /// LOAD_CLASS/UNLOAD_CLASS, stack frames/traces, heap segments, per-object
    /// dumps, per-GC-root-tag). Additive; not parity-compared. Carried from
    /// pass1 counters via the graph. `#[serde(default)]` so pre-v5 reports
    /// (which lack the field) still deserialize.
    #[serde(default)]
    pub record_census: crate::pass2::RecordCensus,
    /// Approximate duplicate-String analysis, present only when `--dup-strings`
    /// was passed; `None` otherwise. Additive; not parity-compared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate_strings: Option<crate::pass2::DupStrings>,
}

/// One step of a single-suspect accumulation path.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct PathStep {
    pub depth: usize,
    pub obj_index_1based: usize,
    pub display_class: String,
    pub retained: u64,
}

/// One hop of the dominator chain from a suspect up toward its GC
/// root. The final hop carries `root_type_label` when the node
/// is itself a GC root.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct RootPathStep {
    pub obj_index_1based: usize,
    pub display_class: String,
    pub retained: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_type_label: Option<String>,
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

/// One node of the FULL multi-level dominator subtree rooted at an accumulation
/// point. Children are the nodes immediately dominated by
/// this one, sorted retained-desc (tie: obj index asc), bounded by the
/// `--detail` max-nodes / max-depth caps.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DomTreeNode {
    pub obj_index_1based: usize,
    pub display_class: String,
    pub shallow: u64,
    pub retained: u64,
    pub children: Vec<DomTreeNode>,
}

/// One node of a "merged shortest paths to GC roots" prefix tree (Eclipse MAT
/// "Merge Shortest Paths"): the dominator chains of all members of a class-group
/// suspect, collapsed by class-at-each-depth. `object_count` is how many member
/// chains pass through this node; `retained` sums those members' retained heap
/// contribution at this node. Additive; not parity-compared.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct MergedPathNode {
    pub display_class: String,
    /// Number of member chains passing through this node.
    pub object_count: u64,
    /// Aggregate retained heap of the objects represented at this node.
    pub retained: u64,
    /// GC-root type label when this node is a root (the chain terminus).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_type_label: Option<String>,
    pub children: Vec<MergedPathNode>,
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
    /// Dominator chain from this single suspect up to its GC root. Only
    /// populated for single suspects; `None` for group suspects (skipped in
    /// JSON). Bounded by the `--detail` root-path max-depth cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_path: Option<Vec<RootPathStep>>,
    /// FULL multi-level dominator subtree rooted at the accumulation point.
    /// `None` when the suspect has no accumulation point (skipped in JSON).
    /// Bounded by the `--detail` max-nodes / max-depth caps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominator_tree: Option<DomTreeNode>,
    /// F: merged shortest paths to GC roots for a class-group suspect — the
    /// member objects' dominator chains collapsed into a class-keyed prefix
    /// tree. `None` for single suspects (they already have `root_path`).
    /// Bounded by the `--detail` max-nodes / max-depth caps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_paths: Option<MergedPathNode>,
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
    /// Retained-size distribution over ALL top-level dominators (additive;
    /// not parity-compared). Empty/zero when there are no top-level dominators.
    #[serde(default)]
    pub size_distribution: TopSizeDistribution,
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
    /// Bounded sample of this thread's GC-thread-local root objects (retained
    /// desc), bounded by the `--detail` per-thread cap. Empty vec when the
    /// thread has no locals. Additive: not part of MAT parity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_objects: Option<Vec<ThreadLocalObj>>,
    /// Shallow heap of the resolved thread object (0 when unresolved). Additive.
    #[serde(default)]
    pub shallow: u64,
    /// Retained heap of the resolved thread object (0 when unresolved). Additive.
    #[serde(default)]
    pub retained: u64,
    /// Largest retained heap among this thread's significant local variables
    /// (0 when frames were not computed). Mirrors MAT's "Max. Locals' Retained
    /// Heap" column. Additive.
    #[serde(default)]
    pub max_local_retained: u64,
    /// Display label of the thread's `contextClassLoader` (e.g.
    /// `java.net.URLClassLoader @ 0x…`), or None when absent/unresolved. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_class_loader: Option<String>,
    /// `java.lang.Thread.daemon`. Additive.
    #[serde(default)]
    pub is_daemon: bool,
    /// `java.lang.Thread.priority`. Additive.
    #[serde(default)]
    pub priority: i32,
    /// Decoded thread state label (e.g. `[alive, runnable]`) from the raw
    /// `threadStatus` bits. Empty when unknown. Additive.
    #[serde(default)]
    pub thread_state: String,
    /// Per-frame significant local variables, interleaved top-first. Populated
    /// only under the opt-in `--thread-locals` flag; empty otherwise. Additive:
    /// not part of MAT parity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub significant_frames: Vec<SignificantFrame>,
}

/// One stack frame plus the significant local-variable objects it retains.
/// Populated only under `--thread-locals`.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct SignificantFrame {
    /// Rendered frame line `class.method (source:line)`.
    pub frame: String,
    /// Significant local objects held at this frame, retained desc.
    pub locals: Vec<SignificantLocal>,
}

/// One significant local-variable object held at a frame.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct SignificantLocal {
    /// Class name of the local object.
    pub display_class: String,
    /// Retained heap of the local object.
    pub retained: u64,
    /// Retained heap as a percentage of the owning thread's retained heap.
    pub pct: f64,
}

/// One sampled GC-thread-local root object held by a thread: its 1-based object
/// index, class name, and footprint.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ThreadLocalObj {
    pub obj_index_1based: usize,
    pub display_class: String,
    pub shallow: u64,
    pub retained: u64,
}

/// Aggregates for the "Threads" section: one entry per resolved stack trace.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ThreadOverview {
    /// Threads with call stacks, sorted by thread serial for determinism.
    pub threads: Vec<ThreadInfo>,
}

/// One power-of-two length bucket in the arrays-by-size histogram. `upper_len`
/// is the inclusive upper bound of the bucket (a power of two): a bucket with
/// `upper_len = 8` counts arrays whose element length is in `5..=8`. The first
/// bucket is `1..=1` (upper_len 1); zero-length arrays are counted separately.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct SizeHistogramBucket {
    pub upper_len: u64,
    pub objects: u64,
    pub shallow: u64,
}

/// Array-length histogram, split by object-arrays vs primitive-arrays, bucketed
/// by power-of-two element length. Always-on; derived from data already in
/// memory (no extra heap scan). Zero-length arrays are tallied separately.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ArraysBySize {
    pub obj_array_buckets: Vec<SizeHistogramBucket>,
    pub prim_array_buckets: Vec<SizeHistogramBucket>,
    pub zero_length_count: u64,
}

/// One "big drop" in the dominator tree: a dominator whose retained heap is
/// much larger than any single child's, i.e. retention concentrates AT this
/// node rather than flowing to one dominated child. A large drop marks a good
/// place to start a leak investigation. Additive; not parity-compared.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct BigDropRow {
    /// 1-based object index of the dominator node.
    pub obj_index_1based: u64,
    pub display_class: String,
    /// Retained heap of the dominator node.
    pub retained: u64,
    /// Number of dominator-tree children of this node.
    pub child_count: u64,
    /// Retained heap of the single largest child (0 if no children).
    pub largest_child_retained: u64,
    /// display class of the largest child (empty if none).
    pub largest_child_class: String,
    /// retained - largest_child_retained: the heap that "drops" here.
    pub drop_bytes: u64,
}

/// The "Big Drops" view: dominators where retained heap concentrates.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct BigDrops {
    /// The retained-heap threshold (bytes) a dominator had to exceed to qualify.
    pub threshold: u64,
    /// Qualifying drops, sorted by drop_bytes descending, capped.
    pub rows: Vec<BigDropRow>,
}

/// One row of the immediate-dominator class rollup: for each dominator class,
/// how many objects it immediately dominates and their aggregate shallow heap.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ImmediateDominatorRow {
    pub dominator_class: String,
    /// Number of distinct dominator objects of this class (with >=1 dom child).
    pub dominator_count: u64,
    /// Number of objects immediately dominated by objects of this class.
    pub dominated_count: u64,
    /// Aggregate shallow heap of those dominator objects.
    pub dominator_shallow: u64,
    /// Aggregate shallow heap of the dominated objects.
    pub dominated_shallow: u64,
}

/// The "Immediate Dominators" view: dominated-object rollup keyed by the
/// dominator's class. Additive; not parity-compared.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ImmediateDominators {
    /// Rows sorted by dominated_shallow descending, capped.
    pub rows: Vec<ImmediateDominatorRow>,
}

/// Always-on dominator-tree analysis grouping Big Drops (#1) and Immediate
/// Dominators (#2), mirroring Eclipse MAT's dominator views. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct DominatorAnalysis {
    pub big_drops: BigDrops,
    pub immediate_dominators: ImmediateDominators,
}

/// One bucket of a fill-ratio (used/capacity) histogram. Ratio expressed in
/// basis points (0..=10000). Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct FillRatioBucket {
    pub lower_ratio_bp: u32,
    pub upper_ratio_bp: u32,
    pub objects: u64,
    pub shallow: u64,
    pub wasted: u64,
}

/// How full collections are (size vs backing-array capacity). `tracked` =
/// collections actually sampled; `total` = all collections seen (tracked <=
/// total when a cap was hit). Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CollectionFillRatio {
    pub tracked: u64,
    pub total: u64,
    pub buckets: Vec<FillRatioBucket>,
}

/// Histogram of collection element counts (reuses SizeHistogramBucket).
/// `empty_count` = collections with size 0. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CollectionsBySize {
    pub tracked: u64,
    pub empty_count: u64,
    pub buckets: Vec<SizeHistogramBucket>,
}

/// Fill ratio of raw object arrays (non-null slots / length). Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ArrayFillRatio {
    pub tracked: u64,
    pub buckets: Vec<FillRatioBucket>,
}

/// Hash-map collision proxy (occupied slots vs size); `wasted` in its buckets
/// is always 0. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct MapCollisionRatio {
    pub tracked: u64,
    pub total: u64,
    pub buckets: Vec<FillRatioBucket>,
}

/// One group of primitive arrays that all hold a single repeated value.
/// Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ConstantArrayRow {
    pub array_class: String,
    pub length: u64,
    pub value: i64,
    pub objects: u64,
    pub shallow: u64,
}

/// Primitive arrays whose every element is the same constant. `truncated` =
/// true when the distinct-group cap was hit and remaining groups were folded
/// into one "other" row. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ConstantPrimitiveArrays {
    pub rows: Vec<ConstantArrayRow>,
    pub truncated: bool,
}

/// One individual array in a "top arrays by shallow bytes" list. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct TopArrayRow {
    pub array_class: String,
    pub length: u64,
    pub shallow: u64,
    pub obj_index_1based: u64,
}

/// One array class in a "top array classes by aggregate shallow bytes" list.
/// Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct TopArrayClassRow {
    pub array_class: String,
    pub objects: u64,
    pub shallow: u64,
}

/// Top arrays for one array category (primitive or object): the largest
/// individual arrays by shallow bytes and the largest array classes by
/// aggregate shallow bytes. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct TopArrays {
    pub top_individual: Vec<TopArrayRow>,
    pub top_by_class: Vec<TopArrayClassRow>,
}

/// Groups the five collection/array views. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CollectionsAnalysis {
    #[serde(default)]
    pub collection_fill_ratio: CollectionFillRatio,
    #[serde(default)]
    pub collections_by_size: CollectionsBySize,
    #[serde(default)]
    pub array_fill_ratio: ArrayFillRatio,
    #[serde(default)]
    pub map_collision_ratio: MapCollisionRatio,
    #[serde(default)]
    pub constant_primitive_arrays: ConstantPrimitiveArrays,
    #[serde(default)]
    pub top_prim_arrays: TopArrays,
    #[serde(default)]
    pub top_obj_arrays: TopArrays,
}

/// One holder `Class#field` ranked by total elements across every container it points at.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct FieldAttributionRow {
    pub holder_class: String,
    pub field: String,
    pub container_kind: String,
    pub total_elements: u64,
    pub total_retained: u64,
    pub container_count: u64,
}

/// One holder `Class#field` whose single largest container is ranked by element count.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct FieldAttributionBiggestRow {
    pub holder_class: String,
    pub field: String,
    pub container_class: String,
    pub elements: u64,
    pub retained: u64,
}

/// Container attribution by holder `Class#field`, present only when `--collections` was passed.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CollectionAttribution {
    pub most_overall: Vec<FieldAttributionRow>,
    pub biggest_single: Vec<FieldAttributionBiggestRow>,
    pub truncated: bool,
}

/// One class row in a reference-statistics histogram. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct RefStatClassRow {
    pub pretty_class: String,
    pub objects: u64,
    pub shallow: u64,
}

/// Statistics for one reference kind (Soft/Weak/Phantom). `kind` is the
/// label. `referent_histogram` = classes of referents grouped/counted.
/// `only_weakly_retained` = referent classes reachable ONLY through the weak
/// edge (idom == u32::MAX). Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ReferenceStats {
    pub kind: String,
    pub reference_instances: u64,
    pub referent_histogram: Vec<RefStatClassRow>,
    pub only_weakly_retained: Vec<RefStatClassRow>,
}

/// The three reference views, each optional (None when that kind is absent).
/// Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ReferencesAnalysis {
    #[serde(default)]
    pub soft: Option<ReferenceStats>,
    #[serde(default)]
    pub weak: Option<ReferenceStats>,
    #[serde(default)]
    pub phantom: Option<ReferenceStats>,
}

/// Scalar indicators of common Java leak patterns. All fields are always
/// computed; zero when the corresponding objects are absent.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize,
         schemars::JsonSchema)]
pub struct LeakIndicators {
    /// Count of anonymous/generated class definitions (names matching `$\d+`,
    /// `$$Lambda$`, `$$Anon`, or `$Proxy` patterns).
    pub anonymous_class_count: u64,
    /// Count of `ThreadLocal$ThreadLocalMap$Entry` instances whose referent
    /// (the ThreadLocal key) has been cleared — the classic thread-local leak signal.
    pub thread_local_null_key_count: u64,
    /// Sum of `capacity` fields across all live `DirectByteBuffer` instances,
    /// representing total off-heap memory tracked by live NIO buffers.
    pub direct_byte_buffer_capacity_sum: u64,
}

/// Schema version for the machine-readable JSON output. Bump on any
/// breaking change to the `Report` shape; the JSON always carries this.
pub const SCHEMA_VERSION: u32 = 2;

/// One allocation site: a distinct HPROF stack-trace serial, its resolved frame
/// lines, and the aggregate footprint of the objects allocated there.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AllocSite {
    pub stack_serial: u32,
    pub frames: Vec<String>,
    pub object_count: u64,
    pub shallow_total: u64,
    pub retained_total: u64,
}

/// Aggregate allocation-site view. `traces_present` is `false` (with an empty
/// `sites`) when the dump carries no allocation stack-trace info (HotSpot
/// writes serial 0 when allocation tracking is off) — reported honestly rather
/// than faked.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AllocSites {
    pub traces_present: bool,
    pub sites: Vec<AllocSite>,
}

/// Full report data model: only bounded aggregates, never a per-object Vec.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Report {
    pub schema_version: u32,
    pub generated: String,
    pub overview: SystemOverview,
    pub leaks: LeakSuspects,
    pub top: TopConsumers,
    pub threads: ThreadOverview,
    /// Eclipse-MAT-style retained-heap-by-class-loader components. Additive;
    /// defaults to empty for round-trip with older JSON.
    #[serde(default)]
    pub top_components: TopComponents,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alloc_sites: Option<AllocSites>,
    /// Power-of-two array-length histogram (object vs primitive arrays).
    /// Always-on; additive, defaults to empty for round-trip with older JSON.
    #[serde(default)]
    pub arrays_by_size: ArraysBySize,
    /// Dominator-tree analysis: Big Drops + Immediate Dominators. Always-on;
    /// additive, defaults to empty for round-trip with older JSON.
    #[serde(default)]
    pub dominator_analysis: DominatorAnalysis,
    /// Field-decode collection & array analysis (fill ratios, size histogram,
    /// map collisions, constant primitive arrays). Always-on; additive,
    /// defaults to empty for round-trip with older JSON.
    #[serde(default)]
    pub collections: CollectionsAnalysis,
    /// Soft/weak/phantom reference statistics. Always-on; additive, defaults to
    /// empty for round-trip with older JSON.
    #[serde(default)]
    pub references: ReferencesAnalysis,
    /// Container attribution by holder `Class#field`, present only when
    /// `--collections` was passed; `None` otherwise. Additive; not parity-compared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_attribution: Option<CollectionAttribution>,
    /// Always-computed scalar leak indicators. Additive; defaults to zero for
    /// round-trip with older JSON.
    #[serde(default)]
    pub leak_indicators: LeakIndicators,
}
