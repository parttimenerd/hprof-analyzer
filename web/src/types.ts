// TypeScript shapes mirroring src/report.rs's serde model. Only the fields the
// UI reads are typed; unknown extra fields are ignored at runtime.

export interface HistRow {
  pretty_class: string;
  instances: number;
  shallow: number;
  retained: number;
  max_instance_shallow: number;
  /** Total inbound reference count for all instances of this class. Optional — absent in older reports. */
  incoming_ref_count?: number;
  loader_id: number;
  // Human-readable class-loader label (the loader's runtime class name; the
  // boot loader is "<boot>"). Absent when unresolved. Preferred over the raw
  // numeric loader_id for display.
  loader_label?: string | null;
  // Dominator chain from the highest-retained instance of this class up to its
  // GC root. Present for the top-20 histogram rows by retained heap.
  // Absent for synthetic rows and rows outside the top-20.
  root_path?: RootPathStep[];
}

export interface GcRootTypeRow {
  root_type: string;
  count: number;
}

// One class entry within a GC-root-retained-by-type row.
export interface GcRootClassRow {
  class_name: string;
  count: number;
  retained: number;
}

// One row of the GC-root retained-by-type table (new in Task 6+).
export interface GcRootRetainedRow {
  root_type: string;
  count: number;
  retained: number;
  top_classes?: GcRootClassRow[];
}

export interface KindStat {
  kind: string;
  objects: number;
  shallow_heap: number;
}

export interface HeapComposition {
  by_kind: KindStat[];
  prim_array_by_type?: KindStat[];
}

export interface DepthBucket {
  depth: number;
  objects: number;
}

export interface RetentionSummary {
  total_retained: number;
  top1_bp: number;
  top10_bp: number;
  top100_bp: number;
  num_objects_ge_1pct: number;
}

export interface SysProp {
  key: string;
  value: string;
}

export interface LoaderRollup {
  loader_label?: string | null;
  loader_id: number;
  class_count: number;
  instances: number;
  shallow: number;
  retained: number;
}

export interface DuplicateClassLoaderRow {
  loader_label: string;
  loader_id: number;
  instances: number;
  shallow: number;
  retained: number;
}

export interface DuplicateClass {
  pretty_class: string;
  loader_count: number;
  loaders: string[];
  total_instances: number;
  total_retained: number;
  per_loader?: DuplicateClassLoaderRow[];
}

export interface RecordCensus {
  utf8_records: number;
  load_class_records: number;
  unload_class_records: number;
  stack_frame_records: number;
  stack_trace_records: number;
  heap_dump_segments: number;
  instance_dumps: number;
  obj_array_dumps: number;
  prim_array_dumps: number;
  class_dumps: number;
  gc_root_tag_counts: [number, number][]; // (tag byte, count)
}

export interface DupStringSample { text: string; count: number; len: number; wasted_bytes: number; }
export interface StrLenBucket { upper_len: number; count: number; }
export interface StrLenStats { min: number; max: number; median: number; total: number; }
export interface StringHolder { class_name: string; string_refs: number; }
export interface CharArrayWasteRow { array_obj_1based: number; length: number; used: number; wasted_bytes: number; }
export interface CharArrayWaste { arrays_examined: number; wasteful_arrays: number; total_wasted_bytes: number; top: CharArrayWasteRow[]; }
export interface DupPrimArrayRow {
  array_class: string;
  duplicated_groups: number;
  wasted_bytes: number;
}

export interface DupArrayHolder {
  class_name: string;
  array_refs: number;
}

export interface DupPrimArrays {
  total_wasted_bytes: number;
  rows: DupPrimArrayRow[];
  top_array_holders?: DupArrayHolder[];
}

export interface BoxedNumberHolder {
  class_name: string;
  boxed_refs: number;
}

export interface BoxedNumberRow {
  pretty_class: string;
  instances: number;
  total_shallow: number;
  pct_of_heap_bp: number;
  avg_shallow: number;
}

export interface HeaderOverheadRow {
  pretty_class: string;
  instances: number;
  header_bytes: number;
  total_header_bytes: number;
  header_pct_of_shallow_bp: number;
  avg_shallow: number;
}

export interface DupStrings {
  distinct_values: number;
  duplicated_values: number;
  total_string_instances: number;
  approx_wasted_bytes: number;
  top_duplicated: DupStringSample[];
  length_histogram: StrLenBucket[];
  length_stats: StrLenStats;
  top_string_holders: StringHolder[];
  top_by_length: DupStringSample[];
  char_array_waste: CharArrayWaste | null;
}

export interface SystemOverview {
  source_name: string;
  file_path: string;
  format: string;
  // JVM version string (e.g. "17.0.9+11"); null when not derivable from the dump.
  jvm_version: string | null;
  // Captured java.lang.System properties. May be empty on modern JDKs where the
  // Properties table is ConcurrentHashMap-backed (empty is normal/expected).
  system_properties: SysProp[];
  file_size: number;
  identifier_size_bits: number;
  compressed_oops: boolean | null;
  dump_creation: number | null;
  total_objects: number;
  total_shallow: number;
  gc_roots: number;
  gc_roots_by_type: GcRootTypeRow[];
  heap_composition: HeapComposition;
  dominator_depth_histogram: DepthBucket[];
  retention_concentration: RetentionSummary;
  classes_loaded: number;
  classloaders_loaded: number;
  unreachable_count: number;
  unreachable_shallow: number;
  unreachable_retained?: number;
  unreachable_composition?: HeapComposition;
  unreachable_histogram: UnreachableClassRow[];
  unreachable_garbage_roots?: UnreachableGarbageRoot[];
  histogram: HistRow[];
  histogram_truncated_to: number | null;
  loader_rollup: LoaderRollup[];
  duplicate_classes: DuplicateClass[];
  // Ratio of unreachable shallow heap to total heap (reachable + unreachable). Range [0, 1].
  heap_fragmentation_ratio?: number;
  // Retained heap share of the single largest class, in integer basis points (100 bp = 1%).
  top_class_concentration_bp?: number;
  // Retained heap grouped by GC root type.
  gc_roots_retained_by_type?: GcRootRetainedRow[];
  // Raw HPROF record-type composition (pass-1 counts); always present.
  record_census: RecordCensus;
  // Opt-in approximate duplicate-String analysis (--find-duplicates). Absent/null otherwise.
  duplicate_strings?: DupStrings | null;
  // Opt-in approximate duplicate-primitive-array analysis (--find-duplicates). Absent/null otherwise.
  duplicate_prim_arrays?: DupPrimArrays | null;
  // Boxed-number wrapper-type rows (java.lang.Integer etc). Empty when none present.
  boxed_numbers?: BoxedNumberRow[];
  // Object-header overhead per class. Empty when no class crosses the threshold.
  header_overhead?: HeaderOverheadRow[];
  // Top classes holding the most boxed-number references. Populated with --collections.
  boxed_number_holders?: BoxedNumberHolder[];
}

export interface PathStep {
  depth: number;
  obj_index_1based: number;
  display_class: string;
  retained: number;
}

export interface DominatedRow {
  obj_index_1based: number;
  display_class: string;
  shallow: number;
  retained: number;
}

// One hop of the dominator chain from a
// suspect up toward its GC root. The final hop carries `root_type_label`.
export interface RootPathStep {
  obj_index_1based: number;
  display_class: string;
  retained: number;
  root_type_label?: string;
  field_edge?: string;
}

// One node of the full multi-level dominator subtree
// rooted at an accumulation point. Recursive via `children`.
export interface DomTreeNode {
  obj_index_1based: number;
  display_class: string;
  shallow: number;
  retained: number;
  children: DomTreeNode[];
}

// One node of the "merged shortest paths to GC roots" prefix tree for a
// class-group suspect: member dominator chains collapsed by class-at-each-depth.
// Recursive via `children`.
export interface MergedPathNode {
  display_class: string;
  object_count: number;
  retained: number;
  root_type_label?: string;
  field_edge?: string;
  children: MergedPathNode[];
}

// One sampled GC-thread-local root object held by a
// thread.
export interface ThreadLocalObj {
  obj_index_1based: number;
  display_class: string;
  shallow: number;
  retained: number;
}

// One row of the ThreadLocal Leak Analyzer per-value-class breakdown.
export interface ThreadLocalLeakRow {
  value_class: string;
  entry_count: number;
  stale_count: number;
  retained: number;
}

// One detected framework's aggregate statistics.
export interface FrameworkAnalysis {
  framework: string;
  instance_count: number;
  total_retained: number;
}

// One aggregated allocation site (a distinct HPROF
// stack-trace serial and the footprint of the objects allocated there).
export interface AllocSite {
  stack_serial: number;
  frames: string[];
  object_count: number;
  shallow_total: number;
  retained_total: number;
}

// aggregate allocation-site view. `traces_present` is
// false (with empty `sites`) when the dump carried no allocation stack-trace info.
export interface AllocSites {
  traces_present: boolean;
  sites: AllocSite[];
}

export interface Suspect {
  is_single: boolean;
  pretty_class: string;
  instance_count: number;
  retained: number;
  shallow: number;
  path: PathStep[];
  accumulation_obj_1based: number | null;
  accumulation_class: string | null;
  accumulation_retained: number | null;
  dominated: DominatedRow[];
  dominated_total_count: number;
  dominated_shown: number;
  dominated_by_class: HistRow[];
  keywords: string[];
  root_type_label: string;
  // dominator chain suspect→…→GC-root.
  // Absent by default.
  root_path?: RootPathStep[];
  // full multi-level dominator subtree at the
  // accumulation point. Absent by default.
  dominator_tree?: DomTreeNode;
  // merged shortest paths to GC roots for a class-group
  // suspect (member chains collapsed by class). Absent for single suspects.
  merged_paths?: MergedPathNode;
}

export interface LeakSuspects {
  total_shallow: number;
  suspects: Suspect[];
}

export interface ObjRow {
  obj_index_1based: number;
  display_class: string;
  shallow: number;
  retained: number;
  pct_bp: number;
  // Dominant incoming reference (`Class#field`) that holds this object. Absent
  // when --collections was off or no attributed field points at it.
  owner?: string | null;
  // Stack-frame holding this object (`ClassName#methodName()`). Present only
  // when the object is a significant local and no field owner was found.
  held_via?: string | null;
}

export interface ClassRow {
  pretty_class: string;
  instances: number;
  retained: number;
}

export interface PackageNode {
  name: string;
  top_dominator_count: number;
  shallow_heap: number;
  retained_heap: number;
  children: PackageNode[];
}

export interface SizeBucket { upper_bytes: number; count: number; }
export interface TopSizeDistribution {
  buckets: SizeBucket[];
  count: number;
  min: number;
  max: number;
  median: number;
  total: number;
}

export interface TopConsumers {
  biggest_objects: ObjRow[];
  biggest_classes: ClassRow[];
  threshold_bp: number;
  biggest_packages: PackageNode;
  size_distribution: TopSizeDistribution;
}

export interface ThreadInfo {
  thread_serial: number;
  name?: string | null;
  class_name: string | null;
  frames: string[];
  // Count of GC-thread-local roots this thread holds that resolve to a live
  // object; a high count flags a thread pinning many objects alive.
  local_root_count: number;
  // bounded sample of this thread's GC-thread-local
  // root objects (retained desc). Absent by default.
  local_objects?: ThreadLocalObj[];
  // Thread-object footprint and always-on properties (mirror MAT columns).
  shallow: number;
  retained: number;
  max_local_retained: number;
  context_class_loader?: string | null;
  is_daemon: boolean;
  priority: number;
  thread_state: string;
  // Per-frame significant locals, interleaved top-first. Empty when locals
  // were not sampled.
  significant_frames?: SignificantFrame[];
}

export interface SignificantFrame {
  frame: string;
  locals: SignificantLocal[];
}

export interface SignificantLocal {
  display_class: string;
  retained: number;
  pct: number;
}

export interface ThreadOverview {
  threads: ThreadInfo[];
}

export interface ComponentClass {
  pretty_class: string;
  retained: number;
}

export interface Component {
  loader_label: string;
  retained: number;
  pct: number;
  top_classes: ComponentClass[];
}

export interface TopComponents {
  components: Component[];
}

export interface SizeHistogramBucket {
  upper_len: number;
  objects: number;
  shallow: number;
}

export interface ArraysBySize {
  obj_array_buckets: SizeHistogramBucket[];
  prim_array_buckets: SizeHistogramBucket[];
  zero_length_count: number;
}

// One "big drop": a dominator whose retained heap concentrates here rather
// than flowing to one dominated child.
export interface BigDropRow {
  obj_index_1based: number;
  display_class: string;
  retained: number;
  child_count: number;
  largest_child_retained: number;
  largest_child_class: string;
  drop_bytes: number;
}

export interface BigDrops {
  threshold: number;
  rows: BigDropRow[];
}

// One immediate-dominator class rollup row.
export interface ImmediateDominatorRow {
  dominator_class: string;
  dominator_count: number;
  dominated_count: number;
  dominator_shallow: number;
  dominated_shallow: number;
}

// One (dominator_class, dominated_class) pair with retained/shallow sizes.
export interface ImmDomPair {
  dominator_class: string;
  dominated_class: string;
  pair_count: number;
  dominated_retained: number;
  dominated_shallow: number;
}

export interface ImmediateDominators {
  rows: ImmediateDominatorRow[];
  pairs?: ImmDomPair[];
}

// Always-on dominator-tree analysis: Big Drops + Immediate Dominators.
export interface DominatorAnalysis {
  big_drops: BigDrops;
  immediate_dominators: ImmediateDominators;
  /** Longest path in the dominator tree (idom-hops from virtual root to deepest node). V25. */
  longest_chain_depth?: number | null;
}

// One row of the per-class unreachable-objects histogram (idom == u32::MAX).
export interface UnreachableClassRow {
  pretty_class: string;
  objects: number;
  shallow: number;
  retained: number;
}

// One node in the garbage-root dominator tree (recursive).
export interface UnreachableGarbageRoot {
  pretty_class: string;
  retained: number;
  objects: number;
  children: UnreachableGarbageRoot[];
}

// One fill-ratio bucket (basis-point range) for collections/arrays/maps.
export interface FillRatioBucket {
  lower_ratio_bp: number;
  upper_ratio_bp: number;
  objects: number;
  shallow: number;
  wasted: number;
}

export interface CollectionFillRatio {
  tracked: number;
  total: number;
  buckets: FillRatioBucket[];
}

export interface CollectionsBySize {
  tracked: number;
  empty_count: number;
  buckets: SizeHistogramBucket[];
}

export interface ArrayFillRatio {
  tracked: number;
  buckets: FillRatioBucket[];
}

export interface MapCollisionRatio {
  tracked: number;
  total: number;
  buckets: FillRatioBucket[];
}

// One group of primitive arrays whose every element is identical.
export interface ConstantArrayRow {
  array_class: string;
  length: number;
  value: number;
  objects: number;
  shallow: number;
  // Dominant incoming reference (`Class#field`) across the group's members.
  // Absent when --collections was off or no field holds them.
  owner?: string | null;
}

export interface ConstantPrimitiveArrays {
  rows: ConstantArrayRow[];
  truncated: boolean;
}

export interface TopArrayRow {
  array_class: string;
  length: number;
  shallow: number;
  obj_index_1based: number;
  // Non-null (occupied) slot count for object arrays; absent for primitive arrays.
  non_null?: number;
  // Primary incoming reference (`Class#field`). Absent when no field edge found.
  owner?: string;
}

export interface TopArrayClassRow {
  array_class: string;
  objects: number;
  shallow: number;
}

export interface TopArrays {
  top_individual: TopArrayRow[];
  top_by_class: TopArrayClassRow[];
}

// Always-on collection/array occupancy analysis.
export interface CollectionsAnalysis {
  collection_fill_ratio: CollectionFillRatio;
  collections_by_size: CollectionsBySize;
  array_fill_ratio: ArrayFillRatio;
  map_collision_ratio: MapCollisionRatio;
  constant_primitive_arrays: ConstantPrimitiveArrays;
  top_prim_arrays?: TopArrays;
  top_obj_arrays?: TopArrays;
  kind_summary?: CollectionKindSummary;
}

// Per-kind rollup over all classified collections.
export interface CollectionKindStat {
  kind: string;
  count: number;
  total_elements: number;
  total_shallow: number;
  max_elements: number;
}
export interface CollectionKindSummary {
  kinds: CollectionKindStat[];
}

// Container Attribution (Class#field): which holder field points at the most
// container memory. Absent when --collections was off.
export interface FieldAttributionRow {
  holder_class: string;
  field: string;
  container_kind: string;
  total_elements: number;
  total_retained: number;
  container_count: number;
  holder_instances: number;
  total_wasted_slots?: number;
  total_wasted_bytes?: number;
}
export interface FieldAttributionBiggestRow {
  holder_class: string;
  field: string;
  container_class: string;
  elements: number;
  capacity: number;
  retained: number;
}
export interface CollectionAttribution {
  most_overall: FieldAttributionRow[];
  biggest_single: FieldAttributionBiggestRow[];
  truncated: boolean;
  // Size-{0,1} collection overhead by Class#field (§46.2).
  tiny_overhead?: TinyCollectionRow[];
}

export interface TinyCollectionRow {
  holder_class: string;
  field: string;
  container_kind: string;
  empty_count: number;
  singleton_count: number;
  overhead_bytes: number;
}

// Fields by Retained Size (Class#field): which holder field retains the most
// memory summed over its pointees. Absent when --collections was off.
export interface FieldBySizeRow {
  holder_class: string;
  field: string;
  pointee_type: string;
  total_retained: number;
  pointees: number;
  holder_instances: number;
  elements?: number;
  category?: string;
}
export interface FieldsBySize {
  rows: FieldBySizeRow[];
  truncated: boolean;
}

export interface ValueTypeShare { type_name: string; count: number; }

export interface BiggestCollectionRow {
  kind: string;
  container_class: string;
  elements: number;
  retained?: number;
  owner?: string;
  dominant_value_type?: string;
  value_type_breakdown?: ValueTypeShare[];
  obj_index_1based?: number;
}
export interface CollectionKindTable { kind: string; rows: BiggestCollectionRow[]; }
export interface BiggestCollections {
  combined: BiggestCollectionRow[];
  by_kind: CollectionKindTable[];
  truncated: boolean;
}
export interface CollectionContentsRow {
  collection_class: string;
  instances: number;
  total_values: number;
  top_value_types: ValueTypeShare[];
}
export interface CollectionContents { rows: CollectionContentsRow[]; truncated: boolean; }

// One class row of a reference referent/only-weakly-retained histogram.
export interface RefStatClassRow {
  pretty_class: string;
  objects: number;
  shallow: number;
  retained?: number;
}

export interface ReferenceStats {
  kind: string;
  reference_instances: number;
  null_referent_count?: number;
  referent_histogram: RefStatClassRow[];
  only_weakly_retained: RefStatClassRow[];
}

// Soft/weak/phantom reference referent analysis. Each kind may be absent.
export interface ReferencesAnalysis {
  soft?: ReferenceStats;
  weak?: ReferenceStats;
  phantom?: ReferenceStats;
}

// Scalar indicators of common Java leak patterns.
export interface LeakIndicators {
  anonymous_class_count: number;
  thread_local_null_key_count: number;
  direct_byte_buffer_capacity_sum: number;
  direct_byte_buffer_count?: number | null;
}

export type TriageSeverity = "info" | "warning" | "critical";

// One fired OOM-triage signal (mirrors src/report/model.rs TriageSignal). Rules
// are evaluated once in Rust; this UI is a dumb formatter over the list.
// `detail` may contain `code spans` in backticks, split into <code> at render.
export interface TriageSignal {
  id: string;
  severity: TriageSeverity;
  title: string;
  detail: string;
  anchor?: string | null;
  anchor_label?: string | null;
  // Reclaimable/attributable bytes used to rank problem signals (§26.2). Present
  // only on quantified problem rules; not rendered — ordering happens in Rust.
  bytes?: number | null;
  // Primary class name to navigate to (PivotBtn/OqlBtn). Present only when the
  // signal has exactly one navigable class.
  nav_class?: string | null;
}

export interface QueryColumn {
  name: string;
}

// Mirrors the Rust QueryValue tagged enum (#[serde(tag="kind", content="v")]).
export type QueryValue =
  | { kind: "null" }
  | { kind: "bool"; v: boolean }
  | { kind: "int"; v: number }
  | { kind: "float"; v: number }
  | { kind: "str"; v: string }
  | { kind: "obj_ref"; v: { index: number; class: string } };

export type VizKind = "table" | "histogram" | "piechart" | "treemap";

export interface VizSpec {
  kind: VizKind;
  label_col?: string;
  value_col?: string;
  cap?: number;
  title?: string;
  name?: string;
}

export interface QueryResult {
  name: string;
  oql: string;
  columns: QueryColumn[];
  rows: QueryValue[][];
  row_count: number;
  truncated: boolean;
  error?: string;
  note?: string;
  viz?: VizSpec;
}

export interface FieldRefStat {
  field_name: string;
  null_count: number;
  non_null_count: number;
  total_retained: number;
}

export interface ClassFieldStats {
  class_name: string;
  instance_count: number;
  ref_fields: FieldRefStat[];
}

export interface FieldStats {
  classes: ClassFieldStats[];
}

export interface Report {
  schema_version: number;
  generated: string;
  /** True when the input gzip stream was truncated; report covers partial data only. */
  truncated_input?: boolean;
  overview: SystemOverview;
  leaks: LeakSuspects;
  top: TopConsumers;
  threads: ThreadOverview;
  // retained-heap-by-class-loader components. Empty by default.
  top_components: TopComponents;
  // aggregated allocation sites. Absent by default.
  alloc_sites?: AllocSites;
  // power-of-two array-length histogram (obj vs prim arrays). Always-on.
  arrays_by_size: ArraysBySize;
  // dominator-tree analysis: Big Drops + Immediate Dominators. Always-on.
  dominator_analysis: DominatorAnalysis;
  // collection/array occupancy analysis. Always-on.
  collections: CollectionsAnalysis;
  // container attribution (Class#field). Absent when --collections was off.
  collection_attribution?: CollectionAttribution;
  // fields ranked by retained size (Class#field). Absent when --collections off.
  fields_by_size?: FieldsBySize;
  biggest_collections?: BiggestCollections;
  collection_contents?: CollectionContents;
  // soft/weak/phantom reference referent analysis. Always-on.
  references: ReferencesAnalysis;
  // Scalar leak-pattern indicators. Always-on; zero fields omitted.
  leak_indicators?: LeakIndicators;
  // Headline reclaimable-bytes figure folding every quantifiable waste source.
  // Absent when every source is zero.
  waste_summary?: WasteSummary;
  // Fired OOM-triage signals, evaluated once in Rust (order = render order).
  triage?: TriageSignal[];
  // Merged top retainers: Class#field + stack-frame, sorted by retained desc (§813).
  top_retainers?: RetainerRow[];
  // Custom OQL query results. Absent/empty when no queries were run.
  queries?: QueryResult[];
  // Which opt-in passes were enabled. Absent on older reports (all flags default false).
  analysis_flags?: AnalysisFlags;
  // Flat object graph for V3/V4 navigation. Only present when --obj-graph was used.
  obj_graph_flat?: ObjGraphFlat;
  // Type-level reference graph (TPFG, V13). Present when --obj-graph was used.
  type_ref_graph?: TypeEdge[];
  // ThreadLocal Leak Analyzer: per-value-class breakdown. Only present when
  // --find-duplicates (or --full-analysis) was passed.
  thread_local_analysis?: ThreadLocalLeakRow[];
  // Detected framework aggregate analyses. Empty when no framework classes present.
  // Always-on; each entry only emitted when its sentinel class is in the heap.
  framework_analysis?: FrameworkAnalysis[];
  // Per-class reference-field statistics. Present only when --field-stats was passed.
  field_stats?: FieldStats;
}

// Which opt-in analysis passes were enabled when the report was generated.
export interface AnalysisFlags {
  find_duplicates: boolean;
  collections: boolean;
  obj_graph?: boolean;
  ref_paths?: boolean;
}

// One row of the merged Top Retainers table (§813).
export interface RetainerRow {
  name: string;
  kind: string;
  retained: number;
}

// One quantifiable waste source: a human label, approximate reclaimable bytes,
// and an optional canonical section slug it drills into.
export interface WasteSource {
  label: string;
  bytes: number;
  anchor?: string;
}

// Headline "reclaimable N bytes" figure folding every waste source. Sources are
// approximate and may overlap slightly; total_bytes is their sum.
export interface WasteSummary {
  total_bytes: number;
  sources: WasteSource[];
}

// ── Object Graph Explorer types (V3 + V4) ─────────────────────────────────────

// One outbound edge from an object in the Reference Graph Explorer (V3).
export interface ObjGraphEdge {
  field_name: string;
  child_idx: number;
  child_class: string;
  child_retained: number;
}

// One inbound edge to an object (reverse reference).
export interface InboundEdge {
  src_idx: number;
  field_name: string;
  src_class: string;
  src_shallow: number;
  src_retained: number;
}

// Capture parameters for object graph extraction.
export interface CaptureParams {
  edge_cap: number;
  size_tier: string;  // "small" | "medium" | "large"
}

// One row in a subtree class breakdown (top-10 by shallow heap within dominated subtree).
export interface SubtreeClassRow {
  class: string;
  instance_count: number;
  total_shallow: number;
}

// One node entry in the flat object graph lookup table.
export interface ObjGraphFlatNode {
  display_class: string;
  shallow: number;
  retained: number;
  edges_unknown?: boolean;
  edges_truncated?: boolean;
  idom?: number;
  dom_subtree_count?: number;   // total objects in dominated subtree (incl. self)
  subtree_classes?: SubtreeClassRow[];  // top-10 classes by shallow within subtree
}

// Flat lookup table powering V3/V4 navigation (reference graph + dominator explorer).
// Only present when --obj-graph was used.
export interface ObjGraphFlat {
  // key = dense index (as string because JSON keys are always strings)
  nodes: Record<string, ObjGraphFlatNode>;
  edges: Record<string, ObjGraphEdge[]>;
  dom_children: Record<string, number[]>;
  root_dom_trees?: [number, DomTreeNode][];
  roots: number[];
  sig_floor_bytes: number;
  inbound_edges?: Record<string, InboundEdge[]>;   // key = dense idx as string
  inbound_truncated?: number[];                     // dense indices where inbound was cut
  capture_params?: CaptureParams;
}

// ── Type-Level Reference Graph (TPFG, V13) ────────────────────────────────────

// One aggregated class-level reference edge in the Type Reference Graph.
// Present in the report only when --obj-graph was used.
export interface TypeEdge {
  src_class: string;
  dst_class: string;
  edge_count: number;
  retained_weight: number;
  /** Top field names (up to 3) via which src references dst, sorted by occurrence. Absent in older reports. */
  top_field_names?: string[];
}

// One directed type-level reference edge's growth between two dumps.
// Mirrors TypeEdgeDiff in src/diff_reports.rs.
export interface TypeEdgeDiff {
  src_class: string;
  dst_class: string;
  count_first: number;
  count_last: number;
  delta_count: number;
  weight_first: number;
  weight_last: number;
  delta_weight: number;
}

declare global {
  interface Window {
    __HPROF_DATA_B64__?: string;
    hprofDecodeText?: (b64: string) => Promise<string>;
  }
}

// ── N-way cross-dump time-series diff (mirrors src/diff_reports.rs) ───────────

// One joined class row across N reports. `retained`/`instances` are length N,
// index 0 = first (baseline), N-1 = last (current); 0 where the class is absent.
export interface SeriesClassRow {
  pretty_class: string;
  retained: number[];
  instances: number[];
  delta_retained: number;
  delta_instances: number;
  // Peak retained across all reports; peak minus baseline (§37.1); and gross
  // per-step growth/shrinkage churn (§37.2). See src/diff_reports.rs.
  peak_retained: number;
  peak_over_baseline: number;
  gross_growth: number;
  gross_shrink: number;
}

// One joined leak-suspect row across N reports.
export interface SeriesSuspectRow {
  pretty_class: string;
  retained: number[];
  delta_retained: number;
  is_new: boolean;
  is_gone: boolean;
}

// The machine-readable N-way cross-dump diff. Every value is an integer; every
// list is deterministically sorted by the Rust engine.
export interface SeriesDiffResult {
  labels: string[];
  total_objects: number[];
  total_shallow: number[];
  delta_total_objects: number;
  delta_total_shallow: number;
  net_delta_retained: number;
  gross_growth_retained: number;
  gross_shrink_retained: number;
  growth_leaders: SeriesClassRow[];
  spike_leaders: SeriesClassRow[];
  new_classes: SeriesClassRow[];
  removed_classes: SeriesClassRow[];
  grown_suspects: SeriesSuspectRow[];
  shrunk_suspects: SeriesSuspectRow[];
  gone_suspects: SeriesSuspectRow[];
  tpfg_diff?: TypeEdgeDiff[];
}

// Tagged envelope embedded by the HTML diff view so the shared bundle can tell
// a diff payload apart from a single-dump Report (which has no `kind` field).
export interface SeriesDiffEnvelope {
  kind: "series-diff";
  diff: SeriesDiffResult;
}
