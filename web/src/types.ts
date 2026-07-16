// TypeScript shapes mirroring src/report.rs's serde model. Only the fields the
// UI reads are typed; unknown extra fields are ignored at runtime.

export interface HistRow {
  pretty_class: string;
  instances: number;
  shallow: number;
  retained: number;
  loader_id: number;
  // Human-readable class-loader label (the loader's runtime class name; the
  // boot loader is "<boot>"). Absent when unresolved. Preferred over the raw
  // numeric loader_id for display.
  loader_label?: string | null;
}

export interface GcRootTypeRow {
  root_type: string;
  count: number;
}

// One row of the GC-root retained-by-type table (new in Task 6+).
export interface GcRootRetainedRow {
  root_type: string;
  count: number;
  retained: number;
}

export interface KindStat {
  kind: string;
  objects: number;
  shallow_heap: number;
}

export interface HeapComposition {
  by_kind: KindStat[];
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
  unreachable_histogram: UnreachableClassRow[];
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

export interface TopConsumers {
  biggest_objects: ObjRow[];
  biggest_classes: ClassRow[];
  threshold_bp: number;
  biggest_packages: PackageNode;
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

export interface ImmediateDominators {
  rows: ImmediateDominatorRow[];
}

// Always-on dominator-tree analysis: Big Drops + Immediate Dominators.
export interface DominatorAnalysis {
  big_drops: BigDrops;
  immediate_dominators: ImmediateDominators;
}

// One row of the per-class unreachable-objects histogram (idom == u32::MAX).
export interface UnreachableClassRow {
  pretty_class: string;
  objects: number;
  shallow: number;
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
}
export interface FieldAttributionBiggestRow {
  holder_class: string;
  field: string;
  container_class: string;
  elements: number;
  retained: number;
}
export interface CollectionAttribution {
  most_overall: FieldAttributionRow[];
  biggest_single: FieldAttributionBiggestRow[];
  truncated: boolean;
}

// One class row of a reference referent/only-weakly-retained histogram.
export interface RefStatClassRow {
  pretty_class: string;
  objects: number;
  shallow: number;
}

export interface ReferenceStats {
  kind: string;
  reference_instances: number;
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
}

export interface Report {
  schema_version: number;
  generated: string;
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
  // soft/weak/phantom reference referent analysis. Always-on.
  references: ReferencesAnalysis;
  // Scalar leak-pattern indicators. Always-on; zero fields omitted.
  leak_indicators?: LeakIndicators;
}

declare global {
  interface Window {
    __HPROF_DATA_B64__?: string;
    hprofDecodeText?: (b64: string) => Promise<string>;
  }
}
