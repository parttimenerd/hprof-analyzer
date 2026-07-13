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
  histogram: HistRow[];
  histogram_truncated_to: number | null;
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
  dominated_by_class: HistRow[];
  keywords: string[];
  root_type_label: string;
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
}

export interface ThreadOverview {
  threads: ThreadInfo[];
}

export interface Report {
  schema_version: number;
  generated: string;
  overview: SystemOverview;
  leaks: LeakSuspects;
  top: TopConsumers;
  threads: ThreadOverview;
}

declare global {
  interface Window {
    __HPROF_DATA_B64__?: string;
    hprofDecodeText?: (b64: string) => Promise<string>;
  }
}
