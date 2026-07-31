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
    /// Retained size within the unreachable forest: the sum of shallow sizes of
    /// every unreachable object dominated by objects of this class, computed by
    /// a dominator pass over the unreachable subgraph (a synthetic root over the
    /// garbage roots). Goes beyond MAT, which discards unreachable objects.
    pub retained: u64,
}

/// One node in the per-garbage-root dominator sub-tree. A garbage root is an
/// unreachable object that has no unreachable predecessor (entry point of a
/// garbage subtree). Children are capped by depth and fan-out. Additive; not
/// parity-compared.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct UnreachableGarbageRoot {
    /// Human-readable class name of the dominating object at this node.
    pub pretty_class: String,
    /// Total retained heap within the subtree rooted at this node (within the
    /// unreachable forest only).
    pub retained: u64,
    /// Number of real objects in the subtree rooted at this node.
    pub objects: u64,
    /// Dominated children, sorted retained-desc, capped.
    #[serde(default)]
    pub children: Vec<UnreachableGarbageRoot>,
}

/// One row of the GC-roots-by-type breakdown: a human-readable root-type label
/// and how many roots carry that HPROF type.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GcRootTypeRow {
    pub root_type: String,
    pub count: u64,
}

/// One class entry within a GC-root-retained-by-type row.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GcRootClassRow {
    pub class_name: String,
    pub count: u64,
    pub retained: u64,
}

/// One row of the GC-root-retained-by-type table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GcRootRetainedRow {
    pub root_type: String,
    pub count: u64,
    pub retained: u64,
    /// Top-5 retained classes for this root type (class name, count, retained bytes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_classes: Vec<GcRootClassRow>,
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
    /// When primitive arrays are present, further split by element type
    /// (e.g. "byte[]", "int[]"). Omitted when empty or only one type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prim_array_by_type: Vec<KindStat>,
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
    /// Exact retained bytes for the top-1 / top-10 / top-100 top-level dominators.
    /// Derive % from these rather than reconstructing from bp to avoid rounding loss.
    #[serde(default)]
    pub top1_retained: u64,
    #[serde(default)]
    pub top10_retained: u64,
    #[serde(default)]
    pub top100_retained: u64,
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

/// One boxed-number class row (java.lang.Integer, Long, etc.) surfaced in the
/// Boxed Numbers section. Data derived from the histogram at report-build time.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct BoxedNumberRow {
    pub pretty_class: String,
    pub instances: u64,
    pub total_shallow: u64,
    /// Share of total reachable shallow heap, in integer basis points (100 bp = 1%).
    pub pct_of_heap_bp: u32,
    /// Average shallow bytes per instance (total_shallow / instances).
    pub avg_shallow: u64,
}

/// One row of the Object Header Overhead breakdown: classes where object headers
/// (12 or 16 bytes each, depending on compressed OOPs) are a significant fraction
/// of the class's total shallow heap. Data derived from the histogram.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct HeaderOverheadRow {
    pub pretty_class: String,
    pub instances: u64,
    /// Object header size in bytes for this JVM: 12 (compressed OOPs / 32-bit) or 16.
    pub header_bytes: u8,
    /// `instances * header_bytes` — total bytes consumed by headers for this class.
    pub total_header_bytes: u64,
    /// Header overhead as a share of the class's total shallow heap,
    /// in integer basis points (100 bp = 1%).
    pub header_pct_of_shallow_bp: u32,
    /// Average shallow bytes per instance (class's total shallow / instances).
    pub avg_shallow: u64,
}

/// One holder-class row for the "who holds the most boxed-number references" ranking.
/// Only populated when `--collections` is also enabled (FieldPlan available).
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct BoxedNumberHolder {
    /// Fully-qualified class name whose instances hold the most references to boxed
    /// primitive types (Integer, Long, Double, etc.).
    pub class_name: String,
    /// Number of object-reference fields pointing at boxed-number instances across all
    /// live instances of this class.
    pub boxed_refs: u64,
}

/// Aggregates for the "System Overview" section.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
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
    /// Total retained heap held within the unreachable forest, computed by a
    /// dominator pass over the unreachable subgraph. Equals the sum of shallow
    /// sizes of all unreachable objects (every unreachable object is dominated
    /// by exactly one garbage root within the forest). Additive; not parity-
    /// compared. 0 when there are no unreachable objects.
    #[serde(default)]
    pub unreachable_retained: u64,
    /// Composition of the unreachable heap by object kind (instances / object
    /// arrays / primitive arrays / class objects), mirroring `heap_composition`
    /// for the reachable heap. Additive; not parity-compared. Empty when there
    /// are no unreachable objects.
    #[serde(default)]
    pub unreachable_composition: HeapComposition,
    /// Top garbage-root dominator subtrees in the unreachable forest, sorted by
    /// retained desc and capped (top-5 roots × depth-4). Each entry is the root
    /// of a subtree of unreachable objects with no reachable predecessor; the
    /// tree structure reflects the dominator relationships within that garbage
    /// cluster. Additive; not parity-compared.
    #[serde(default)]
    pub unreachable_garbage_roots: Vec<UnreachableGarbageRoot>,
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
    /// Approximate duplicate-String analysis, present only when `--find-duplicates`
    /// was passed; `None` otherwise. Additive; not parity-compared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate_strings: Option<crate::pass2::DupStrings>,
    /// Approximate duplicate-primitive-array analysis, present only when
    /// `--find-duplicates` was passed; `None` otherwise. Additive; not parity-compared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate_prim_arrays: Option<crate::pass2::DupPrimArrays>,
    /// Boxed-number classes (java.lang.Integer, Long, etc.) found in the histogram,
    /// sorted by total shallow heap descending. Empty when no boxed types appear.
    /// Additive; not parity-compared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boxed_numbers: Vec<BoxedNumberRow>,
    /// Per-class object-header overhead: classes where headers account for a large
    /// fraction of shallow heap or represent a large absolute overhead. Top-30
    /// sorted by total_header_bytes descending. Additive; not parity-compared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header_overhead: Vec<HeaderOverheadRow>,
    /// Top classes holding the most references to boxed-number objects
    /// (Integer, Long, etc.). Populated only when `--collections` is on.
    /// Additive; not parity-compared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boxed_number_holders: Vec<BoxedNumberHolder>,
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
    /// Name of the field on this object that points to the next hop (parent's
    /// field → child). Only present when `--ref-paths` was set. Empty string
    /// means "no field name available" (class edge, array element, synthetic
    /// thread-local edge).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_edge: Option<String>,
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

/// Outbound edge from one object to another (Reference Graph Explorer / V3).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ObjGraphEdge {
    #[serde(default)]
    pub field_name: String,
    pub child_idx: u32,
    pub child_class: String,
    pub child_retained: u64,
}

/// One inbound reference captured in the static report snapshot.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct InboundEdge {
    /// Dense index of the object holding the reference to this node.
    pub src_idx: u32,
    /// Field name on `src` that points to this node, or "" if unnamed.
    pub field_name: String,
    /// Display class of `src`.
    pub src_class: String,
    /// Shallow heap of `src`.
    pub src_shallow: u64,
    /// Retained heap of `src`.
    pub src_retained: u64,
}

/// Parameters used when capturing the object graph snapshot.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CaptureParams {
    /// Max edges per object (outbound and inbound).
    pub edge_cap: usize,
    /// Human-readable tier name: "small", "medium", or "large".
    pub size_tier: String,
}

/// One node in the flat object graph lookup table.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ObjGraphFlatNode {
    pub display_class: String,
    pub shallow: u64,
    pub retained: u64,
    #[serde(default)]
    pub edges_unknown: bool,
    #[serde(default)]
    pub edges_truncated: bool,
    pub idom: Option<u32>,
}

/// One aggregated type-level reference edge for the TPFG (V13).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TypeEdge {
    pub src_class: String,
    pub dst_class: String,
    pub edge_count: u64,
    /// Sum of src-object retained / out_degree for all instances of this edge type.
    pub retained_weight: u64,
}

/// Flat lookup table powering V3 + V4 navigation (object graph + dominator explorer).
/// Only present when --obj-graph is used.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ObjGraphFlat {
    /// All significant nodes (retained >= sig_floor_bytes). Key = dense index (u32).
    pub nodes: std::collections::HashMap<u32, ObjGraphFlatNode>,
    /// Outbound edges for captured nodes. Key = dense index (u32).
    pub edges: std::collections::HashMap<u32, Vec<ObjGraphEdge>>,
    /// Immediate dominator children for all significant nodes. Key = parent dense index.
    pub dom_children: std::collections::HashMap<u32, Vec<u32>>,
    /// Pre-built depth-3 dominator trees for top-20 root objects (for SVG mode in V4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub root_dom_trees: Vec<(u32, DomTreeNode)>,
    /// Dense indices of dominator roots (idom == vroot).
    pub roots: Vec<u32>,
    /// Minimum retained bytes to be included as a significant node.
    pub sig_floor_bytes: u64,
    /// Inbound reference snapshot (who points to each captured node).
    /// Key = dense index (as string in JSON). Only present for nodes in the capture set.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub inbound_edges: std::collections::HashMap<u32, Vec<InboundEdge>>,
    /// Dense indices of nodes where inbound edges were truncated at edge_cap.
    #[serde(default, skip_serializing_if = "std::collections::HashSet::is_empty")]
    pub inbound_truncated: std::collections::HashSet<u32>,
    /// Capture parameters (tier name, edge_cap) for the UI indicator.
    #[serde(default)]
    pub capture_params: CaptureParams,
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
    /// Field name on the parent node that points to this child; only present
    /// when `--ref-paths` was set and a consistent field name was found across
    /// all chains that pass through this node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_edge: Option<String>,
    pub children: Vec<MergedPathNode>,
}

/// One leak suspect (single large object or class group).
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
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
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct LeakSuspects {
    pub total_shallow: u64,
    pub suspects: Vec<Suspect>,
}

/// One row of "Biggest Objects".
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
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
    /// Dominant incoming reference (`Class#field`) that holds this object.
    /// `None` when `--collections` was off or no attributed field points at it.
    #[serde(default)]
    pub owner: Option<String>,
    /// Stack-frame holding this object (`ClassName#methodName()`), when the
    /// object is a significant local in a thread's stack frame and no field
    /// owner was found. `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_via: Option<String>,
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
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
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
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
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
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
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
    /// Retained heap as a percentage of total reachable shallow heap (same
    /// basis as every other "% Heap" figure in the report).
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
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
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

/// One (dominator_class, dominated_class) pair for the V5 two-sided sankey.
/// Powers "who holds X" (rows where dominated_class == target) and
/// "what does X hold" (rows where dominator_class == target) from a single dataset.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ImmDomPair {
    pub dominator_class: String,
    pub dominated_class: String,
    /// Number of (dominator, dominated) object pairs counted.
    pub pair_count: u64,
    /// Aggregate retained heap of the dominated objects.
    pub dominated_retained: u64,
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
    /// Per-(dominator_class, dominated_class) pairs for the two-sided sankey.
    /// Sorted by dominated_retained descending, capped at IMDOM_PAIRS_CAP.
    /// Additive; defaults to empty for round-trip with older JSON.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pairs: Vec<ImmDomPair>,
}

/// Always-on dominator-tree analysis grouping Big Drops (#1) and Immediate
/// Dominators (#2), mirroring Eclipse MAT's dominator views. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct DominatorAnalysis {
    pub big_drops: BigDrops,
    pub immediate_dominators: ImmediateDominators,
    /// Longest path in the dominator tree (number of idom-hops from the virtual
    /// root to the deepest node). A value > 10,000 indicates a linked-list-shaped
    /// data structure. 0 for an empty heap. V25.
    #[serde(default)]
    pub longest_chain_depth: u32,
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
    /// Dominant incoming reference (`Class#field`) across the group's member
    /// arrays. `None` when `--collections` was off or no field holds them.
    #[serde(default)]
    pub owner: Option<String>,
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
    /// Non-null (occupied) slot count for object arrays; `None` for primitive
    /// arrays (every slot is always occupied). Additive.
    #[serde(default)]
    pub non_null: Option<u64>,
    /// Primary incoming reference (`Class#field`) that points at this array.
    /// Resolved unconditionally from instance-dump field edges (first-wins).
    /// `None` when no field edge references this array. Additive.
    #[serde(default)]
    pub owner: Option<String>,
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
    #[serde(default)]
    pub kind_summary: CollectionKindSummary,
}

/// One reclaimable-waste source in the Waste Summary: a human label, the
/// approximate reclaimable bytes, and an optional anchor to the section that
/// details it. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct WasteSource {
    /// Human-readable source label, e.g. "Under-filled collections".
    pub label: String,
    /// Approximate reclaimable bytes attributed to this source.
    pub bytes: u64,
    /// Canonical section slug this source drills into (e.g. "collections"),
    /// or None when there is no dedicated section.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
}

/// A single headline "reclaimable N bytes" figure folding every waste source
/// the report can quantify (under-filled collections & object arrays, duplicate
/// String values, String backing-array slack, duplicate primitive arrays). The
/// sources are approximate and may overlap slightly; `total_bytes` is their sum.
/// Present only when at least one source is nonzero. Additive; not part of MAT
/// parity comparison.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct WasteSummary {
    /// Sum of every source's bytes (the headline "reclaimable" figure).
    pub total_bytes: u64,
    /// Per-source breakdown, sorted by bytes desc. Only nonzero sources.
    pub sources: Vec<WasteSource>,
}

/// One collection-kind's aggregate stats. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CollectionKindStat {
    pub kind: String,        // "list"/"map"/"set"/"deque"/"queue"/"tree"
    pub count: u64,          // number of collections of this kind (with a readable size)
    pub total_elements: u64, // sum of sizes
    pub total_shallow: u64,  // sum of the collection instances' shallow bytes
    pub max_elements: u64,   // largest single size in this kind
}

/// Per-kind rollup over all classified collections. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CollectionKindSummary {
    pub kinds: Vec<CollectionKindStat>,
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
    /// Live-instance count of `holder_class` from the System-Overview class
    /// histogram (matched by prettified class name), or `0` when the holder
    /// class has no histogram row. Additive.
    #[serde(default)]
    pub holder_instances: u64,
    /// Sum of (capacity - elements) across all distinct containers reached by
    /// this field. Counts empty slots, not bytes. Zero for classified
    /// collections (their capacity is not cheaply available). Additive.
    #[serde(default)]
    pub total_wasted_slots: u64,
    /// `total_wasted_slots × object-reference width (bytes)` — the actual bytes
    /// of backing-array capacity that is null/unused. More directly comparable
    /// than slot count when array types differ. Additive.
    #[serde(default)]
    pub total_wasted_bytes: u64,
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
    /// Backing-array length (slots) of the single largest container:
    /// `elements` = used, `capacity` = slots. Real for arrays; for classified
    /// collections this equals `elements` (degenerate — see the field-decode
    /// note; the backing array's true length is not cheaply joinable). Additive.
    #[serde(default)]
    pub capacity: u64,
    /// Kind of the single largest container (same labels as `FieldAttributionRow::container_kind`).
    #[serde(default)]
    pub container_kind: String,
}

/// Container attribution by holder `Class#field`, present only when `--collections` was passed.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CollectionAttribution {
    pub most_overall: Vec<FieldAttributionRow>,
    pub biggest_single: Vec<FieldAttributionBiggestRow>,
    /// Class#field pairs owning the most size-{0,1} collections,
    /// ranked by wrapper-overhead bytes.
    #[serde(default)]
    pub tiny_overhead: Vec<TinyCollectionRow>,
    pub truncated: bool,
}

/// One row in the tiny-collection overhead ranking (§46.2).
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct TinyCollectionRow {
    pub holder_class: String,
    pub field: String,
    pub container_kind: String,
    pub empty_count: u64,
    pub singleton_count: u64,
    /// Estimated overhead bytes: (empty_count + singleton_count) × 80.
    pub overhead_bytes: u64,
}

/// One holder `Class#field` (with declared field type) ranked by the total
/// retained size of everything the field points at, summed over all live
/// holder instances. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct FieldBySizeRow {
    /// Pretty holder class name.
    pub holder_class: String,
    /// Field name.
    pub field: String,
    /// Runtime class of what the field points at (dominant type by retained
    /// size). HPROF does not record declared reference-field types, so this is
    /// the concrete pointee class; `varies` when the field points at multiple
    /// types and no single one dominates.
    pub pointee_type: String,
    /// Sum of retained size over distinct pointees of this `Class#field`.
    pub total_retained: u64,
    /// Number of distinct pointees (non-null field slots) for this field.
    pub pointees: u64,
    /// Live-instance count of `holder_class` from the class histogram, or `0`.
    pub holder_instances: u64,
    /// Total element count over container pointees of this field (`0` for
    /// non-container fields). Additive.
    #[serde(default)]
    pub elements: u64,
    /// Pointee category: `"collection"`, `"array"`, or `"object"`, derived from
    /// the dominant runtime pointee type. Additive.
    #[serde(default)]
    pub category: String,
}

/// `Class#field` holders ranked by total retained size of their pointees.
/// Present only when `--collections` was passed. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct FieldsBySize {
    pub rows: Vec<FieldBySizeRow>,
    pub truncated: bool,
}

/// One runtime-type share within a collection's element/value tally. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ValueTypeShare {
    /// Pretty runtime class name of the element/value.
    pub type_name: String,
    /// Number of element slots of this type in the group.
    pub count: u64,
}

/// One individual collection instance in the "biggest collections" listing.
/// Basics (kind/container_class/elements) are always present; the remaining
/// fields are filled only under `--collections`. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct BiggestCollectionRow {
    pub kind: String,
    pub container_class: String,
    pub elements: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dominant_value_type: Option<String>,
    /// Top element/value runtime types by count (Level 2). Empty when
    /// `--collections` was off or no element types were tallied.
    #[serde(default)]
    pub value_type_breakdown: Vec<ValueTypeShare>,
}

/// Biggest collections of one kind (e.g. all Maps), ranked. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CollectionKindTable {
    pub kind: String,
    pub rows: Vec<BiggestCollectionRow>,
}

/// The largest individual collection instances: a combined ranking plus a
/// per-kind breakdown. Present whenever collections were classified (basics
/// always; extra columns only under `--collections`). Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct BiggestCollections {
    pub combined: Vec<BiggestCollectionRow>,
    pub by_kind: Vec<CollectionKindTable>,
    pub truncated: bool,
}

/// One collection *class* (e.g. `java.util.HashMap`) aggregated across all its
/// instances: instance count, total element/value slots, and the top runtime
/// element/value types globally. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CollectionContentsRow {
    pub collection_class: String,
    pub instances: u64,
    pub total_values: u64,
    pub top_value_types: Vec<ValueTypeShare>,
}

/// Global "what's in your collections" breakdown, one row per collection class.
/// Present only under `--collections`. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CollectionContents {
    pub rows: Vec<CollectionContentsRow>,
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
    #[serde(default)]
    pub retained: u64,
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
    /// Number of reference objects whose referent field is null (referent was GC'd).
    #[serde(default)]
    pub null_referent_count: u64,
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
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
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

/// Severity of a fired OOM-triage signal. Currently carried for future HTML
/// styling (colour per severity); rule order, not severity, drives ordering.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum TriageSeverity {
    Info,
    Warning,
    Critical,
}

/// One fired OOM-triage signal. Rules evaluate the finished `Report` once (see
/// `triage.rs`) and emit these; both the Markdown and HTML renderers are dumb
/// formatters over `Report.triage`, so rule logic lives in exactly one place.
///
/// `detail` may contain `` `code spans` `` in backticks: Markdown keeps them
/// verbatim, HTML splits them into `<code>` elements.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TriageSignal {
    /// Stable slug identifying the rule, e.g. `"off-heap"`.
    pub id: String,
    pub severity: TriageSeverity,
    /// Bold label, e.g. `"Off-heap (DirectByteBuffer)"`.
    pub title: String,
    /// One-sentence explanation; may contain backtick code spans.
    pub detail: String,
    /// Target section id to link to, e.g. `"leak-indicators"`. `None` = no link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// Link text for `anchor`, e.g. `"Leak Indicators"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_label: Option<String>,
    /// Reclaimable/attributable bytes this signal quantifies, when the rule has a
    /// concrete figure (e.g. wasted collection bytes, duplicate-String waste). Used
    /// to rank problem signals by impact; `None` for orientation signals and rules
    /// without a byte figure. Not rendered — ordering only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

/// Schema version for the machine-readable JSON output. Bump on any
/// breaking change to the `Report` shape; the JSON always carries this.
pub const SCHEMA_VERSION: u32 = 11;

/// One detected framework's aggregate statistics.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FrameworkAnalysis {
    /// Human-readable framework name (e.g. "Hibernate", "Spring").
    pub framework: String,
    /// Count of live instances of the sentinel class (or subclasses).
    pub instance_count: u32,
    /// Total retained heap across all sentinel class instances, in bytes.
    pub total_retained: u64,
}

/// One row of the ThreadLocal Leak Analyzer breakdown: value class name,
/// entry counts (total + stale), and total retained heap of the stored values.
/// A stale entry is one whose weak referent (the ThreadLocal key) has been
/// GC'd but whose value is still strongly held — a classic TL leak signal.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ThreadLocalLeakRow {
    /// Pretty class name of the stored value object.
    pub value_class: String,
    /// Total number of ThreadLocalMap$Entry objects with this value class.
    pub entry_count: u32,
    /// Number of entries whose key (referent) has been GC'd (stale entries).
    pub stale_count: u32,
    /// Sum of retained heap across all value objects of this class.
    pub retained: u64,
}

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
    /// Holder `Class#field` ranked by total retained size of their pointees,
    /// present only when `--collections` was passed; `None` otherwise. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields_by_size: Option<FieldsBySize>,
    /// Largest individual collection instances. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub biggest_collections: Option<BiggestCollections>,
    /// Global per-collection-class value-type breakdown. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_contents: Option<CollectionContents>,
    /// Always-computed scalar leak indicators. Additive; defaults to zero for
    /// round-trip with older JSON.
    #[serde(default)]
    pub leak_indicators: LeakIndicators,
    /// One headline "reclaimable N bytes" figure folding every quantifiable
    /// waste source. Present only when at least one source is nonzero. Additive;
    /// `#[serde(default)]` keeps older JSON (which lacks the field) loadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waste_summary: Option<WasteSummary>,
    /// Fired OOM-triage signals, evaluated once over the finished report by the
    /// rule framework in `triage.rs`. Order is the registry order (render order).
    /// `#[serde(default)]` keeps pre-v4 JSON (which lacks the field) loadable.
    #[serde(default)]
    pub triage: Vec<TriageSignal>,
    /// Merged top-retainers: `Class#field` holders + `Class#method()` stack
    /// frames, sorted by total retained descending, capped at 20.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_retainers: Vec<RetainerRow>,
    /// Custom OQL query results (empty unless --query/--query-file was given).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queries: Vec<crate::query::model::QueryResult>,
    /// Flat object graph for V3/V4 navigation. None when --obj-graph not used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obj_graph_flat: Option<ObjGraphFlat>,
    /// Type-level reference graph (TPFG). Present when --obj-graph is used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_ref_graph: Vec<TypeEdge>,
    /// Which opt-in analysis passes were enabled. Additive; defaults to all-false
    /// for round-trip with older JSON (which lacks the field).
    #[serde(default)]
    pub analysis_flags: AnalysisFlags,
    /// ThreadLocal Leak Analyzer: per-value-class breakdown of
    /// `ThreadLocalMap$Entry` objects — entry counts, stale counts (null key),
    /// and total retained heap. Populated only when `--find-duplicates` (or
    /// `--full-analysis`) is passed; empty otherwise. Additive; not
    /// parity-compared. `#[serde(default)]` keeps older JSON loadable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thread_local_analysis: Vec<ThreadLocalLeakRow>,
    /// Detected framework aggregate analyses. Empty when no framework classes present.
    /// Always-on; each entry is only emitted when its sentinel class is in the heap.
    /// `#[serde(default)]` keeps older JSON loadable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub framework_analysis: Vec<FrameworkAnalysis>,
}

/// Which opt-in analysis passes were enabled when the report was generated.
/// Used by the viewer to show context-appropriate "not run" messages.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct AnalysisFlags {
    /// `--find-duplicates` was passed (duplicate string/array analysis).
    #[serde(default)]
    pub find_duplicates: bool,
    /// `--collections` was passed (fill-ratio & waste analysis).
    #[serde(default)]
    pub collections: bool,
    /// `--obj-graph` (or `--full-analysis`) was passed (reference/dominator graph capture).
    #[serde(default)]
    pub obj_graph: bool,
    /// `--ref-paths` was passed (field-name labels on edges).
    #[serde(default)]
    pub ref_paths: bool,
}

/// One row of the merged Top Retainers table (§813).
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct RetainerRow {
    pub name: String,
    pub kind: String,
    pub retained: u64,
}

#[cfg(test)]
mod model_completeness {
    use super::*;
    #[test]
    fn obj_graph_flat_has_inbound_fields() {
        let flat = ObjGraphFlat::default();
        let _ = flat.inbound_edges;
        let _ = flat.inbound_truncated;
        let _ = &flat.capture_params;
    }
}
