//! Pass-2 data model: graph output struct + inbound-builder state.

#![allow(dead_code)]

use std::{
    collections::HashMap,
    io::{self, ErrorKind},
};

use crate::{reader::HprofReader, types::tags, vbyte};

use super::Pass2;

/// Inbound CSR block size: one sampled byte-offset per INB_BLOCK nodes.
/// Each node's predecessor slice is count-prefixed so it is self-delimiting;
/// dominator Phase-1 seeks to the block start then scans-skips to node w.
/// Trades ~K/2 extra vbyte skips per lookup for dropping the full per-node
/// offset array (n+1 u32 = ~2GB) down to (n/K) u32.
pub const INB_BLOCK: usize = 16;

// ── Graph output struct ────────────────────────────────────────────────────

/// A resolved thread stack trace, produced at Graph-build time by resolving
/// STACK_TRACE/STACK_FRAME string-ids and class-serials against pass1 tables
/// (which are dropped before the report stage). Small (one per thread), off the
/// per-object RSS budget.
#[derive(Debug, Clone, Default)]
pub struct ThreadStack {
    /// HPROF thread serial from the STACK_TRACE record (0 = none).
    pub thread_serial: u32,
    /// Object index of the owning `java.lang.Thread` (u32::MAX = unresolved).
    pub thread_obj_idx: u32,
    /// Frames top-first, each pre-rendered as `class.method (source:line)`.
    pub frames: Vec<String>,
}

/// Per-thread properties decoded from the `java.lang.Thread` instance blob
/// (name + the always-on overview scalars). Bounded by #threads. The
/// `context_loader_addr` is left as a raw object address here and resolved to a
/// display label at report-build time (where the loader tables live).
#[derive(Debug, Clone, Default)]
pub struct ThreadProps {
    /// Decoded thread name (empty if the name String could not be resolved).
    pub name: String,
    /// `java.lang.Thread.daemon` (defaults false if the field is absent).
    pub is_daemon: bool,
    /// `java.lang.Thread.priority` (defaults 0 if the field is absent).
    pub priority: i32,
    /// `java.lang.Thread.threadStatus` raw JVMTI status bits (0 if absent).
    pub thread_status: i32,
    /// `java.lang.Thread.contextClassLoader` object address (0 = none/absent).
    pub context_loader_addr: u64,
}

/// Raw HPROF record-type census for the dump: top-level record counts plus a
/// per-GC-root-tag breakdown. Additive metadata surfaced in System Overview;
/// not parity-compared. Populated from pass1 counters.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct RecordCensus {
    pub utf8_records: u64,
    pub load_class_records: u64,
    pub unload_class_records: u64,
    pub stack_frame_records: u64,
    pub stack_trace_records: u64,
    pub heap_dump_segments: u64,
    pub instance_dumps: u64,
    pub obj_array_dumps: u64,
    pub prim_array_dumps: u64,
    pub class_dumps: u64,
    /// (root sub-tag byte, count), sorted by count desc then tag asc for stable output.
    pub gc_root_tag_counts: Vec<(u8, u64)>,
}

/// Approximate duplicate-`java.lang.String` analysis. Each String value is
/// decoded, hashed to a 64-bit value, and only the hash + length + occurrence
/// count is retained — the decoded bytes are dropped immediately, so RSS stays
/// bounded regardless of dump size. Hash collisions merge distinct values (an
/// accepted approximation). The unit of dedup is the String INSTANCE: two
/// String instances with the same decoded value count as a duplicate even
/// though they usually hold separate backing arrays. Opt-in via `--find-duplicates`.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct DupStrings {
    /// Distinct decoded String values (by 64-bit hash; collisions merge — accepted approximation).
    pub distinct_values: u64,
    /// Distinct values that occur in more than one String instance.
    pub duplicated_values: u64,
    /// Total java.lang.String instances scanned.
    pub total_string_instances: u64,
    /// Approx bytes wasted by duplication: Σ over duplicated values of (count-1)*first_seen_len.
    pub approx_wasted_bytes: u64,
    /// Top-N most-duplicated String values with exact (truncated) text, sorted by
    /// occurrence count desc then text asc. Only values with count > 1.
    #[serde(default)]
    pub top_duplicated: Vec<DupStringSample>,
    /// Power-of-two histogram of decoded String lengths (bytes), one entry per
    /// distinct value. Sorted by `upper_len` ascending.
    #[serde(default)]
    pub length_histogram: Vec<StrLenBucket>,
    /// Summary stats over distinct-value lengths (bytes).
    #[serde(default)]
    pub length_stats: StrLenStats,
    /// Top-N owning classes by the number of `java.lang.String` instances their
    /// instances reference. Sorted by `string_refs` desc then class name asc.
    #[serde(default)]
    pub top_string_holders: Vec<StringHolder>,
    /// Top-N longest distinct String values by decoded byte length, sorted by
    /// len desc then text asc. Only populated with `--find-duplicates`.
    #[serde(default)]
    pub top_by_length: Vec<DupStringSample>,
    /// Wasted space in char[]/byte[] arrays backing Strings. `None` unless
    /// `--find-duplicates` computed it.
    #[serde(default)]
    pub char_array_waste: Option<CharArrayWaste>,
}

/// One of the most-duplicated String values: its exact text (truncated to
/// `MAX_STR_SAMPLE` bytes), how many String instances share the value, the
/// decoded byte length of the value, and the approximate wasted bytes.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct DupStringSample {
    /// Exact decoded text, truncated to at most `MAX_STR_SAMPLE` bytes on a char boundary.
    pub text: String,
    /// Number of String instances sharing this value.
    pub count: u64,
    /// Decoded byte length of the value (pre-truncation).
    pub len: u32,
    /// Approx wasted bytes for this value: (count - 1) * len.
    pub wasted_bytes: u64,
}

/// One wasteful char[] backing a String (String uses fewer bytes than the
/// array length). Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CharArrayWasteRow {
    pub array_obj_1based: usize,
    pub length: u64,
    pub used: u64,
    pub wasted_bytes: u64,
}

/// Waste in char[]/byte[] arrays backing Strings. `top` sorted by
/// wasted_bytes desc, capped. Additive.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CharArrayWaste {
    pub arrays_examined: u64,
    pub wasteful_arrays: u64,
    pub total_wasted_bytes: u64,
    pub top: Vec<CharArrayWasteRow>,
}

/// One power-of-two bucket of the String-length histogram. `upper_len` is the
/// inclusive upper bound (a power of two); a value of length `l` falls in the
/// smallest bucket whose `upper_len >= l`. `count` is the number of distinct
/// String values in this bucket.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct StrLenBucket {
    pub upper_len: u32,
    pub count: u64,
}

/// Summary stats over distinct-value String lengths (bytes). `Default` = zeros.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct StrLenStats {
    pub min: u32,
    pub max: u32,
    pub median: u32,
    /// Sum of all distinct-value lengths (bytes).
    pub total: u64,
}

/// One owning class and how many `java.lang.String` instances its instances
/// reference (across all object-reference fields).
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct StringHolder {
    pub class_name: String,
    pub string_refs: u64,
}

/// The complete reference graph plus all report-facing metadata produced by
/// pass 2. Its large per-object arrays (`shallow`, `class_idx`, the forward CSR)
/// are the dominant RSS consumers, so several are compressed/freed early during
/// `build` — see the field docs. The dominator/retained stages fill `idom`,
/// `retained`, and `has_same_class_ancestor` afterward.
/// One raw container-attribution record produced by field-decode under
/// `--collections`. Carries the DENSE object index (retained size is filled
/// later and looked up in build_model) plus pre-resolved owned name Strings
/// (class_map/strings die right after field-decode). Runtime-only, not serialized.
#[derive(Clone)]
pub struct AttributionRaw {
    pub container_idx: u32,
    pub holder_class: String,
    pub field: String,
    pub container_kind: u8,
    pub container_class: String,
    pub elements: u64,
    /// Backing-array length (slots): `elements` = used, `capacity` = slots.
    /// Real for arrays; equals `elements` for classified collections (see the
    /// field-decode container-insert note).
    pub capacity: u64,
}

/// One raw `Class#field` group produced by field-decode under `--collections`:
/// a holder class + field and the DENSE indices of the distinct objects that
/// field points at. Retained size and the dominant runtime pointee type are
/// computed later in build_model (where `idom`/`retained`/`class_idx` are
/// known). Bounded by caps applied in field-decode. Runtime-only, not serialized.
#[derive(Clone)]
pub struct FieldSizeRaw {
    pub holder_class: String,
    pub field: String,
    /// Distinct pointee dense object indices (deduped, capped).
    pub pointee_indices: Vec<u32>,
}

/// Raw per-collection element/value tally produced by field-decode under
/// `--collections`. Carries the collection instance's DENSE object index, its
/// kind byte and pretty container-class name, plus the DENSE object indices of
/// its non-null element slots (deduped is NOT required — counts matter). Runtime
/// element types are resolved later in build_model from `class_idx`. Bounded by
/// a per-collection slot cap. Runtime-only, not serialized.
#[derive(Clone)]
pub struct CollValuesRaw {
    pub container_idx: u32,
    pub kind: u8,
    pub container_class: String,
    /// Primary incoming `Class#field` referrer, resolved from the holder-edge
    /// map at emit time. `None` when the collection has no attributed owner.
    pub owner: Option<String>,
    /// Dense object indices of non-null element slots (capped per collection).
    pub value_indices: Vec<u32>,
}

pub struct Graph {
    /// Object count = number of live nodes in the graph (indexes 0..n).
    pub n: usize,
    /// Dump format string from the HPROF header (e.g. "JAVA PROFILE 1.0.2").
    pub format: String,
    /// Total size of the dump file in bytes.
    pub file_size: u64,
    /// File basename the dump was opened from (see `file_path` for the full path).
    pub source_name: String,
    /// Full path/name the dump was opened from (Pass1::run's `path`). Distinct
    /// from `source_name`, which is only the file basename.
    pub file_path: String,
    /// HPROF identifier size in bytes (4 or 8), straight from the header.
    pub id_size: u8,
    /// Object-reference size in bytes as detected in pass2. Equals `id_size`
    /// unless compressed OOPs shrink 8-byte ids to 4-byte refs.
    pub ref_size: u8,
    /// Header base timestamp (millis since Unix epoch), 0 if absent/unknown.
    pub header_timestamp_ms: u64,
    /// Object indices that are GC roots, sorted ascending. Includes both real
    /// HPROF roots and synthetic system-class roots (`synthetic_root_count` of
    /// the latter).
    pub gc_root_indices: Vec<u32>,
    /// Per-root HPROF sub-tag, aligned 1:1 with `gc_root_indices` (same order).
    /// A representative type when an index has multiple root records (the
    /// minimum sub-tag, deterministically). `heap::ROOT_SYSTEM_CLASS` (0x00)
    /// marks synthetic system-class roots. Powers `gc_roots_by_type` (B1) and
    /// the default why-alive line, so it is carried unconditionally.
    #[allow(dead_code)]
    pub gc_root_types: Vec<u8>,
    /// Per-object MAT shallow size in bytes, 1:1 with object indices 0..n.
    /// Compressed + emptied early in `build` (its dense ~2GB Vec is restored
    /// from the blob before the retained stage) to keep it off the RSS peak.
    pub shallow: Vec<u32>,
    /// Per-object class-histogram row index, 1:1 with objects. Keyed by
    /// CLASS-OBJECT identity (loader-distinct), so `class_names[class_idx[i]]`
    /// is object i's class name. Also compressed/emptied early like `shallow`.
    pub class_idx: Vec<u32>,
    /// Class-histogram row names, indexed by the values in `class_idx`.
    pub class_names: Vec<String>,
    /// Class-loader object address per histogram row, aligned 1:1 with
    /// `class_names`. 0 = boot/bootstrap loader. Synthetic rows (primitive
    /// arrays, the single java/lang/Class row) are boot-loaded (0). Powers the
    /// class-loader count and per-loader grouping; a per-ROW array (not
    /// per-object) so it costs O(#classes), never O(#objects).
    pub class_loader_id: Vec<u64>,
    /// Class-loader OBJECT address -> class NAME of that loader object, for the
    /// distinct non-boot loaders seen across histogram rows (`class_loader_id`).
    /// Lets the report layer render a human loader label instead of a raw
    /// address. Boot loader (addr 0) is absent here and labeled `<boot>` by the
    /// report layer. Bounded by #distinct loaders, so O(#loaders), not O(#objects).
    pub loader_labels: std::collections::HashMap<u64, String>,
    /// Resolved thread stack traces (one per STACK_TRACE with frames), built
    /// from pass1's STACK_FRAME/STACK_TRACE tables. Small; feeds Thread Overview
    /// and leak-suspect stack context. Empty when the dump carries no traces.
    pub thread_stacks: Vec<ThreadStack>,
    /// Decoded `java.lang.Thread` properties per HPROF thread serial: name plus
    /// the always-on overview scalars (daemon / priority / threadStatus /
    /// contextClassLoader address). Populated by a bounded multi-pass worklist in
    /// `Pass2::build` (thread objects → their name String → the String's
    /// char/byte array → decoded text; scalars read straight from the thread
    /// blob). Bounded by the number of threads (hundreds), so it never touches the
    /// per-object RSS budget. Absent serials render as an unnamed thread.
    pub thread_props: std::collections::HashMap<u32, ThreadProps>,
    /// Per-thread count of GC-thread-local roots that resolved to a live object
    /// (thread_serial -> #resolved locals). Filled from `p1.thread_local_pairs`
    /// during synthetic-edge resolution, using the SAME guard (thread/local both
    /// resolve to indices and are distinct). Bounded by #threads (hundreds), so
    /// it never touches the per-object RSS budget on multi-GB dumps.
    pub thread_local_counts: std::collections::HashMap<u32, u64>,
    /// Bounded per-thread sample of GC-thread-local root object indices
    /// (thread_serial -> Vec of local object indices, capped at
    /// `opts.thread_locals_per_thread`). ONLY populated when the opt-in
    /// `--thread-locals` flag is set; otherwise stays empty (zero memory on the
    /// default path). Bounded by #threads * cap, so off the per-object budget.
    pub thread_local_samples: std::collections::HashMap<u32, Vec<u32>>,
    /// Gated per-thread (frame_number, local object index) pairs used to build
    /// MAT's per-frame significant-locals interleave. `frame_number == u32::MAX`
    /// means the local has no associated stack frame (JNI local / native stack /
    /// thread block). ONLY populated when `--thread-locals` is set; otherwise
    /// empty (zero cost on the default path). Bounded by #threads * cap.
    pub thread_local_frame_samples: std::collections::HashMap<u32, Vec<(u32, u32)>>,
    /// Decoded JVM system properties (java.lang.System static `props`), as
    /// (key, value) pairs sorted by key. Captured by `resolve_system_properties`
    /// via a bounded multi-pass worklist over ONE Properties/Hashtable object.
    /// Capped at 4096 entries. Empty when the props object is absent or its
    /// layout does not match the Hashtable form (graceful fallback — never
    /// garbage). Bounded, so off the per-object RSS budget on multi-GB dumps.
    pub system_properties: Vec<(String, String)>,
    /// Derived JVM version string: prefers the `java.vm.version` property, else
    /// `java.version`, else None. Populated even when the full property table
    /// could not be decoded (both keys are extracted from `system_properties`).
    pub jvm_version: Option<String>,
    /// Object index of a class object -> the histogram row of the class it
    /// represents. Sparse: absent for non-class objects.
    pub class_obj_class_idx: HashMap<u32, u32>, // class-obj index -> class-histogram row (sparse; absent = not a class obj)
    // Forward CSR: node i's out-edges are `fwd_targets[fwd_offsets[i]..fwd_offsets[i+1]]`.
    /// CSR row pointers, len n+1: `fwd_offsets[i]..fwd_offsets[i+1]` slices node
    /// i's out-edge targets in `fwd_targets`. Built via prefix-sum of out-degrees.
    pub fwd_offsets: Vec<u32>,
    /// Flat concatenation of every node's out-edge target indices, sliced by
    /// `fwd_offsets`. Chunked so the transpose can free consumed chunks incrementally,
    /// capping the (fwd_targets + inb_flat) coexistence peak.
    pub fwd_targets: crate::chunkvec::ChunkU32,
    /// Number of GC roots added synthetically (system class roots, etc.)
    /// Reported GC roots = gc_root_indices.len() - synthetic_root_count
    pub synthetic_root_count: usize,
    /// MAT-formula instance shallow size of `java/lang/ClassLoader`, if that
    /// class exists in the dump. MAT materializes a synthetic bootstrap
    /// `<system class loader>` object at address 0x0 (no HPROF record) of this
    /// class; the report layer injects one such object's count + shallow so
    /// `total_objects`/`total_shallow` match MAT bit-exactly. `None` = the
    /// class is absent, inject nothing.
    pub system_classloader_shallow: Option<u32>,
    // Filled by later passes (dominator / retained-size stages).
    /// Immediate-dominator index per object (dominator tree). Empty until the
    /// dominator stage fills it.
    pub idom: Vec<u32>,
    /// Retained size in bytes per object. Empty until the retained-size stage.
    pub retained: Vec<u64>,
    /// Marks objects that have an ancestor of the same class in the dominator
    /// tree (used to suppress double-counting in class-level retained roll-ups).
    pub has_same_class_ancestor: crate::bitset::Bitset,
    /// Per-object HPROF allocation stack-trace serial, 1:1 with objects. Only
    /// populated when `--alloc-sites` is set (moved out of `p1` during build);
    /// empty otherwise. Consumed by the report's alloc-site aggregation and not
    /// needed afterward.
    pub alloc_stack_serial: Vec<u32>,
    /// Distinct non-zero alloc stack-trace serials pre-resolved into their frame
    /// lines, built during `build` while the STACK_FRAME/STACK_TRACE tables are
    /// still alive. `Some` only when `--alloc-sites` is set; `None` otherwise.
    pub alloc_frames_by_serial: Option<std::collections::HashMap<u32, Vec<String>>>,
    /// Raw HPROF record-type census (per-record-type + per-GC-root-tag counts)
    /// carried from pass1's cheap scalar counters. Additive; not parity-compared.
    pub record_census: RecordCensus,
    /// Approximate duplicate-`java.lang.String` analysis. `Some` only when the
    /// opt-in `--find-duplicates` flag is set; `None` otherwise (zero extra work,
    /// zero RSS on the default path). See [`DupStrings`].
    pub dup_strings: Option<DupStrings>,
    /// Approximate duplicate-primitive-array analysis. `Some` only when
    /// `--find-duplicates` was passed; `None` otherwise.
    pub dup_prim_arrays: Option<super::DupPrimArrays>,
    /// Top classes holding the most references to boxed-number objects.
    /// Populated only when `--collections` was passed; empty otherwise.
    pub boxed_number_holders: Vec<crate::report::BoxedNumberHolder>,
    /// Power-of-two array-length histogram (object vs primitive arrays), folded
    /// during pass2 from `p1.elem_count`/`p1.kind` before those arrays are freed.
    /// Always populated; additive, not parity-compared.
    pub arrays_by_size: crate::report::ArraysBySize,
    /// Field-decode collection & array analysis. Always populated; additive,
    /// not parity-compared. See [`crate::report::CollectionsAnalysis`].
    pub collections: crate::report::CollectionsAnalysis,
    /// Soft/weak/phantom reference statistics. Always populated; additive, not
    /// parity-compared. See [`crate::report::ReferencesAnalysis`].
    pub references: crate::report::ReferencesAnalysis,
    /// Capped referent object indices per reference kind [soft, weak, phantom],
    /// consumed in `build_model` to compute `only_weakly_retained` via `idom`.
    /// Not serialized (runtime-only helper).
    pub reference_referent_idx: [Vec<u32>; 3],
    /// Count of null referents per reference kind (0=Soft, 1=Weak, 2=Phantom).
    /// Populated at scan time in fielddecode.rs; used by build_references.
    /// Not serialized (runtime-only helper).
    pub reference_null_referent_count: [u64; 3],
    /// Raw container-attribution records from field-decode under `--collections`;
    /// `None` when the flag was off. Consumed in build_model to attach retained
    /// sizes and aggregate. Not serialized.
    pub collection_attribution_raw: Option<Vec<AttributionRaw>>,
    /// True when the holder-edge or container-record cap was hit (attribution
    /// data is a bounded sample). Not serialized.
    pub collection_attribution_truncated: bool,
    /// Raw `Class#field` → pointee-index records from field-decode under
    /// `--collections`; `None` when the flag was off. Consumed in build_model to
    /// sum retained size per field. Not serialized.
    pub fields_by_size_raw: Option<Vec<FieldSizeRaw>>,
    /// Raw per-collection element tallies (dense value indices). `None` when
    /// `--collections` was off. Consumed by build_model, then dropped.
    pub coll_values_raw: Option<Vec<CollValuesRaw>>,
    /// Node/Entry wrapper unwrap map: dense object index of a wrapper (e.g.
    /// `HashMap$Node`) → (key dense idx, value dense idx), where `u32::MAX`
    /// means null or unresolvable. `None` when `--collections` was off.
    /// Consumed by build_model to show real K/V types in Biggest Collections.
    pub node_kv: Option<std::collections::HashMap<u32, (u32, u32)>>,
    /// Per-edge field-name index, parallel to `fwd_targets` (same length).
    /// `fwd_field_name_idx[pos]` is the index into `field_name_pool` for the
    /// field that caused the edge at `fwd_targets[pos]`. 0 means "no name" (class
    /// edges, array element edges, synthetic thread-local edges). Populated only
    /// when `--ref-paths` is set; `None` on the default path (zero RSS).
    pub fwd_field_name_idx: Option<Vec<u16>>,
    /// Deduped field-name strings indexed by `fwd_field_name_idx`.
    /// Pool[0] is always "" (no name). Populated only when `--ref-paths` is set.
    pub field_name_pool: Option<Vec<String>>,
    /// Sum of `capacity` fields across all live `java/nio/DirectByteBuffer`
    /// instances. 0 when no such instances are found or the field cannot be
    /// resolved. Computed unconditionally during the pass2 field-decode scan.
    #[allow(dead_code)]
    pub direct_byte_buffer_capacity_sum: u64,
    /// Count of live `ThreadLocal$ThreadLocalMap$Entry` instances whose weak
    /// `referent` (the ThreadLocal key) is null — the classic thread-local leak
    /// signature (cleared key, value still strongly held). Computed during the
    /// pass2 field-decode scan, where referent nullness is directly observable;
    /// it cannot be recovered from the forward CSR because a null referent is
    /// simply an absent edge, indistinguishable from any other missing target.
    #[allow(dead_code)]
    pub thread_local_null_key_count: u64,
    /// Retained sizes within the unreachable forest, computed by the
    /// `unreachable_retained` stage in `main.rs` while the forward CSR is still
    /// alive. `None` until that stage runs (or when there are no unreachable
    /// objects). Consumed by `build_system_overview`; a bounded per-class
    /// aggregate, so it carries no per-object RSS into the report phase.
    pub unreachable_retained: Option<crate::unreachable_retained::UnreachableRetained>,
    /// Bounded outbound-edge snapshot for object-graph click-through.
    /// Captured before `fwd_offsets`/`fwd_targets` are consumed during inbound
    /// CSR construction. `None` until `capture_obj_graph_edges` is called.
    pub obj_graph_edges: Option<ObjGraphCapture>,
}

// ── Object-graph click-through capture ──────────────────────────────────────

/// Bounded snapshot of outbound edges for the top-N objects by shallow heap,
/// captured before the forward-edge CSR is consumed during inbound construction.
pub struct ObjGraphCapture {
    /// src dense-idx → [(dst dense-idx, field_name_pool_idx)].
    /// field_name_pool_idx 0 means unnamed (array element, class edge, synthetic).
    pub edges: std::collections::HashMap<u32, Vec<(u32, u16)>>,
    /// Deduped field-name strings; index 0 is always "" (unnamed).
    pub field_name_pool: Vec<String>,
    /// Dense indices that were captured as sources (top-K by shallow heap).
    pub captured: std::collections::HashSet<u32>,
}

impl ObjGraphCapture {
    pub fn empty() -> Self {
        Self {
            edges: Default::default(),
            field_name_pool: vec![String::new()],
            captured: Default::default(),
        }
    }
}

/// Capture outbound edges for the top-`top_n` objects by shallow heap, storing
/// at most `edge_cap` edges per source node. Must be called while `g.fwd_offsets`
/// and `g.fwd_targets` are still alive (before inbound CSR construction consumes
/// them). Field names are included when `g.fwd_field_name_idx` is populated
/// (`--ref-paths`); otherwise all edges are stored as unnamed (index 0).
pub fn capture_obj_graph_edges(g: &Graph, top_n: usize, edge_cap: usize) -> ObjGraphCapture {
    // Use g.n (object count) — g.shallow may be compressed/empty at call time.
    let n = if g.shallow.is_empty() { g.n } else { g.shallow.len() };
    if n == 0 || g.fwd_offsets.is_empty() {
        return ObjGraphCapture::empty();
    }

    // When top_n covers all objects, skip the expensive sort.
    let mut indices: Vec<u32> = (0..n as u32).collect();
    if top_n < n {
        indices.sort_unstable_by(|&a, &b| g.shallow[b as usize].cmp(&g.shallow[a as usize]));
        indices.truncate(top_n);
    }

    let mut cap = ObjGraphCapture::empty();
    // name_str → pool index (0 reserved for "").
    let mut name_map: std::collections::HashMap<String, u16> = std::collections::HashMap::new();
    name_map.insert(String::new(), 0u16);

    let fwd_names = g.fwd_field_name_idx.as_ref();
    let name_pool = g.field_name_pool.as_ref();

    for src in &indices {
        let src = *src;
        let s = src as usize;
        let start = g.fwd_offsets[s] as usize;
        let end = g.fwd_offsets[s + 1] as usize;
        cap.captured.insert(src);
        let mut row: Vec<(u32, u16)> = Vec::with_capacity((end - start).min(edge_cap));
        for pos in start..end {
            if row.len() >= edge_cap {
                break;
            }
            let dst = g.fwd_targets.get(pos);
            let name_idx: u16 = match (fwd_names, name_pool) {
                (Some(idx_vec), Some(pool)) => {
                    let ni = idx_vec[pos] as usize;
                    let name: &str = if ni < pool.len() { &pool[ni] } else { "" };
                    if name.is_empty() {
                        0u16
                    } else {
                        let next = name_map.len() as u16;
                        let idx = *name_map.entry(name.to_owned()).or_insert(next);
                        if idx as usize >= cap.field_name_pool.len() {
                            cap.field_name_pool.push(name.to_owned());
                        }
                        idx
                    }
                }
                _ => 0u16,
            };
            row.push((dst, name_idx));
        }
        if !row.is_empty() {
            cap.edges.insert(src, row);
        }
    }
    cap
}

/// Deferred inbound-CSR construction. Built by `Pass2::build` with everything
/// needed to run the inbound scan + delta-encode later (after rpo frees its
/// arrays), keeping the ~5.5GB inbound CSR off the rpo-phase RSS peak.
#[allow(dead_code)]
pub struct InboundBuilder {
    pub(crate) source: crate::source::HprofSource,
    pub(crate) id_size: u8,
    pub(crate) n: usize,
    /// Live id_map as constructed by `build`; taken by `compress_id_map`.
    pub(crate) id_map: Option<crate::id_map::IdMap>,
    /// Compressed id_map (blob, element_count); set by `compress_id_map`.
    pub(crate) id_map_c: Option<(Vec<u8>, usize)>,
    pub(crate) id_map_codec: crate::cvec::Codec,
    pub(crate) class_addr_to_hist: HashMap<u64, u32>,
    pub(crate) field_plans_dense: Vec<super::FieldPlan>,
    /// Prefix-summed inbound start cursors (in_degree after prefix-sum), len n.
    pub(crate) in_cursors: Vec<u32>,
    pub(crate) total_inb: u64,
    /// Synthetic thread->local edges (src,dst), already deduped.
    pub(crate) synthetic_edges: Vec<(u32, u32)>,
}

/// Captured from `InboundBuilder` before it is consumed by `build_mat_scan`,
/// so that a post-inbound HPROF rescan can rebuild the forward-edge CSR without
/// keeping `fwd_targets` alive across the inbound peak window.
pub struct MatOutboundRescanCtx {
    pub source: crate::source::HprofSource,
    pub id_size: u8,
    /// Compressed id_map blob + element count + codec.
    pub id_map_c: Option<(Vec<u8>, usize)>,
    pub id_map_codec: crate::cvec::Codec,
    pub class_addr_to_hist: HashMap<u64, u32>,
    pub field_plans_dense: Vec<super::FieldPlan>,
}

impl InboundBuilder {
    /// Extract the data needed for a later outbound-only HPROF rescan.
    /// Must be called after `compress_id_map`. id_map_c blob is cloned
    /// (~0.5 GB for large dumps). class_addr_to_hist and field_plans_dense are
    /// cloned (cheap: ~1 MB total). The builder retains full copies of all data.
    pub fn take_for_outbound_rescan(&mut self) -> MatOutboundRescanCtx {
        MatOutboundRescanCtx {
            source: self.source.clone(),
            id_size: self.id_size,
            id_map_c: self.id_map_c.clone(),
            id_map_codec: self.id_map_codec,
            class_addr_to_hist: self.class_addr_to_hist.clone(),
            field_plans_dense: self.field_plans_dense.clone(),
        }
    }

    /// Compress the live id_map into a blob and free the dense Vec, so the
    /// ~4.1GB addr array is off the rpo-phase RSS peak. No-op for Codec::None.
    pub fn compress_id_map(&mut self, codec: crate::cvec::Codec) -> io::Result<()> {
        self.id_map_codec = codec;
        if codec == crate::cvec::Codec::None {
            return Ok(());
        }
        if let Some(m) = self.id_map.take() {
            let (blob, len) = m.compress(codec)?;
            self.id_map_c = Some((blob, len));
        }
        Ok(())
    }

    /// Build the inbound CSR by transposing the already-computed forward CSR,
    /// avoiding a third full-file scan. Requires the forward CSR
    /// (fwd_offsets/fwd_targets) and dfn (pre-order) to still be alive.
    /// id_map, class_addrs, and field_plans stored in self are no longer needed
    /// and are freed before the Phase-4 encode.
    ///
    /// Memory peak: fwd_offsets + fwd_targets + inb_flat + in_cursors + dfn.
    /// On a 34 GB dump this is ~17 GB, well within the thinkstation's budget
    /// but higher than the old deferred-scan path (~9 GB). The trade-off is
    /// eliminating the ~234 s fourth full-file scan entirely.
    pub fn build_from_fwd(
        self,
        fwd_offsets: Vec<u32>,
        mut fwd_targets: crate::chunkvec::ChunkU32,
        dfn: &[u32],
    ) -> io::Result<(Vec<u64>, Vec<u8>)> {
        let InboundBuilder {
            n,
            in_cursors,
            total_inb,
            synthetic_edges: _,
            // The rest are only needed by the file-scan path; drop them early to
            // free the id_map (~4 GB) before the inb_flat alloc.
            id_map,
            id_map_c,
            class_addr_to_hist,
            field_plans_dense,
            ..
        } = self;

        drop(id_map);
        drop(id_map_c);
        drop(class_addr_to_hist);
        drop(field_plans_dense);

        let mut inb_flat = crate::chunkvec::ChunkU32::zeroed(total_inb as usize);
        if crate::trace::enabled() {
            eprintln!(
                "[trace-rss] inbound (fwd-transpose): total_inb={} edges, inb_flat={} MB",
                total_inb,
                (total_inb as usize * 4) / (1024 * 1024)
            );
        }
        crate::trace::probe("inbound fwd-transpose: after inb_flat alloc");

        let mut in_cursors = in_cursors;

        // Transpose the forward CSR: for each src and each of its fwd targets
        // dst, write src into inb_flat at in_cursors[dst] and advance the cursor.
        // `in_cursors[i]` starts as the cumulative prefix-sum START for node i
        // and advances to the END as edges are written.
        // fwd_targets chunks are freed as the read pointer advances past each
        // 256 MB boundary — capping (fwd_targets + inb_flat) coexistence peak.
        let n_nodes = fwd_offsets.len().saturating_sub(1);
        let mut buf: Vec<u32> = Vec::with_capacity(4096);
        let mut next_fwd_free: usize = 1 << 26; // first chunk boundary = 64 M u32 = 256 MB
        // MADV_DONTNEED fwd_offsets pages as src advances past page boundaries.
        // fwd_offsets[0..src] is dead after processing src; freeing pages
        // immediately counteracts the inb_flat page faults during the transpose.
        #[cfg(target_os = "linux")]
        let fwd_off_ptr = fwd_offsets.as_ptr();
        #[cfg(target_os = "linux")]
        let mut next_off_dontneed: usize = 1 << 10; // first page boundary = 1024 u32 = 4 KB
        for src in 0..n_nodes {
            let lo = fwd_offsets[src] as usize;
            let hi = fwd_offsets[src + 1] as usize;
            if lo == hi {
                continue; // no out-edges — skip copy_range + buf iteration
            }
            // Free fully-consumed fwd_targets chunks as the lo pointer advances.
            if lo >= next_fwd_free {
                fwd_targets.free_below(lo);
                next_fwd_free = ((lo >> 26) + 1) << 26; // next chunk boundary
            }
            // DONTNEED consumed fwd_offsets pages as src advances past page boundaries.
            #[cfg(target_os = "linux")]
            if src >= next_off_dontneed {
                let pages_end = src & !(1024 - 1); // align down to 4KB page
                let len = pages_end * std::mem::size_of::<u32>();
                if len > 0 {
                    unsafe {
                        libc::madvise(fwd_off_ptr as *mut libc::c_void, len, libc::MADV_DONTNEED);
                    }
                }
                next_off_dontneed = pages_end + 1024; // advance by one page
            }
            // Use range_slice for zero-copy access when the range fits in one
            // chunk; fall back to copy_range for cross-chunk adjacency lists.
            let targets: &[u32] = if let Some(sl) = fwd_targets.range_slice(lo, hi) {
                sl
            } else {
                fwd_targets.copy_range(lo, hi, &mut buf);
                &buf
            };
            for &dst in targets {
                let dst = dst as usize;
                inb_flat.set(in_cursors[dst] as usize, src as u32);
                in_cursors[dst] += 1;
            }
        }
        // fwd_offsets and fwd_targets are no longer needed; free them before
        // Phase 4 allocates inb_data to reduce the coexistence peak.
        drop(fwd_offsets);
        drop(fwd_targets);
        crate::trace::trim();
        crate::trace::probe("inbound fwd-transpose: after transpose loop");

        // Synthetic edges are already included in fwd_targets (they were
        // appended before the B3 restore in pass2b), so we must NOT add them
        // again here — doing so would double-count them and overflow in_cursors.

        crate::trace::probe("inbound: before Phase-4 (after fwd-transpose)");
        Self::encode_phase4(n, total_inb, in_cursors, inb_flat, dfn)
    }

    /// Run the inbound scan + Phase-4 encode. Returns (inb_offsets, inb_data).
    #[cfg(test)]
    pub fn build(self, dfn: &[u32]) -> io::Result<(Vec<u64>, Vec<u8>)> {
        let InboundBuilder {
            source,
            id_size,
            n,
            id_map,
            id_map_c,
            id_map_codec,
            class_addr_to_hist,
            field_plans_dense,
            mut in_cursors,
            total_inb,
            synthetic_edges,
            ..
        } = self;

        // Reconstruct the id_map: either it was left live (Codec::None) or it
        // was compressed by `compress_id_map` and must be decompressed here.
        // This decompress spike lands at inbound-start, after rpo freed its
        // dfn/vertex arrays, so it stays below the rpo peak.
        let id_map = match id_map {
            Some(m) => m,
            None => {
                let (blob, len) = id_map_c.expect("id_map neither live nor compressed");
                crate::id_map::IdMap::from_compressed(&blob, len, id_map_codec)?
            }
        };

        // -- Alloc flat inbound array (deferred until after rpo freed its arrays) --
        // Chunked backing store so Phase-4 can free consumed chunks incrementally,
        // avoiding the inb_flat+inb_data coexistence that was the global RSS peak.
        let mut inb_flat = crate::chunkvec::ChunkU32::zeroed(total_inb as usize);
        if crate::trace::enabled() {
            eprintln!(
                "[trace-rss] inbound 2b: total_inb={} edges, inb_flat={} MB",
                total_inb,
                (total_inb as usize * 4) / (1024 * 1024)
            );
        }
        crate::trace::probe("inbound 2b: after inb_flat alloc");

        // -- Sub-pass 2b scan: fill INBOUND edges only --
        {
            let mut r = source.open()?;
            let mut scratch: Vec<u8> = Vec::with_capacity(4096);
            let mut fwd_t_stub: crate::chunkvec::ChunkU32 = crate::chunkvec::ChunkU32::zeroed(0);
            let mut fwd_offsets_stub: Vec<u32> = Vec::new();
            loop {
                let tag = match r.u1() {
                    Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
                    other => other?,
                };
                let _ts = r.u4()?;
                let length = r.u4()? as u64;
                match tag {
                    tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                        Pass2::fill_heap_2b(
                            &mut r,
                            id_size,
                            length,
                            &id_map,
                            &class_addr_to_hist,
                            &field_plans_dense,
                            &[], // named plans not needed for inbound pass
                            false,
                            true,
                            &mut fwd_t_stub,
                            &mut fwd_offsets_stub,
                            &mut None, // no field name index for inbound pass
                            &mut Vec::new(),
                            &mut std::collections::HashMap::new(),
                            &mut inb_flat,
                            &mut in_cursors,
                            &mut scratch,
                        )?;
                    }
                    tags::HEAP_DUMP_END => break,
                    _ => {
                        r.skip(length)?;
                    }
                }
            }
        }

        // id_map / class_addr_to_hist / field_plans_dense are consumed only by the 2b scan
        // above. Free them now (id_map alone is ~4.1 GB at 514M objects) before
        // the Phase-4 encode allocates inb_data, trimming the global RSS peak.
        drop(id_map);
        drop(class_addr_to_hist);
        drop(field_plans_dense);

        // Synthetic thread->local INBOUND edges.
        for &(src, dst) in &synthetic_edges {
            inb_flat.set(in_cursors[dst as usize] as usize, src);
            in_cursors[dst as usize] += 1;
        }

        crate::trace::probe("inbound: before Phase-4 (after 2b scan + drops)");
        Self::encode_phase4(n, total_inb, in_cursors, inb_flat, dfn)
    }

    /// Phase 4: translate node indices to pre-order numbers via `dfn`, sort,
    /// dedup, and delta-encode into the blocked inbound CSR. Shared by both
    /// the file-scan path (`build`) and the fwd-transpose path (`build_from_fwd`).
    fn encode_phase4(
        n: usize,
        total_inb: u64,
        in_cursors: Vec<u32>,
        mut inb_flat: crate::chunkvec::ChunkU32,
        dfn: &[u32],
    ) -> io::Result<(Vec<u64>, Vec<u8>)> {
        #[allow(clippy::redundant_locals)]
        let in_cursors = in_cursors;
        // -- Phase 4: Build inbound CSR (blocked offsets + count-prefixed data) --
        // inb_block_off[b] = byte offset where node (b*INB_BLOCK)'s slice begins.
        // Each node's slice = vbyte(count) then `count` vbyte pre-order deltas.
        let mut inb_block_off: Vec<u64> = Vec::with_capacity(n / INB_BLOCK + 2);
        let mut inb_data: Vec<u8> = Vec::new();
        // Pre-allocate inb_data to ~2.5 bytes/edge (observed: 4173 MB / 1653 M
        // edges ≈ 2.53 B/edge on the 34 GB benchmark dump) to avoid doubling
        // past 2× the true size. Without this the Vec doubles from ~4 GB to an
        // 8 GB capacity, wasting ~4 GB of RSS through Phase 4.
        // Cap at 6 GB so we don't over-commit on small dumps.
        // On 32-bit targets (wasm32) usize::MAX is the upper bound instead.
        #[cfg(target_pointer_width = "64")]
        let six_gb: usize = 6 * 1024 * 1024 * 1024;
        #[cfg(not(target_pointer_width = "64"))]
        let six_gb: usize = usize::MAX;
        let inb_data_cap = ((total_inb as usize).saturating_mul(5) / 2).min(six_gb);
        inb_data.reserve(inb_data_cap);

        // CSR is contiguous: start[i] = end of node i-1 = in_cursors[i-1] after fill.
        let mut start = 0usize;
        // Reusable per-node scratch: copy each node's inbound slice out of the
        // chunked store so we can sort/dedup it, then free chunks behind us.
        let mut nb: Vec<u32> = Vec::new();
        // Free consumed chunks every ~256 M slots crossed (one chunk).
        let mut next_free_at: usize = 1 << 26;
        for i in 0..n {
            let end = in_cursors[i] as usize; // in_cursors[i] = end offset after fill

            // Record a sampled block offset at each block boundary (BEFORE the
            // count-prefix), so a lookup for any node in the block can seek here
            // and scan-skip forward to the target node.
            if i % INB_BLOCK == 0 {
                inb_block_off.push(inb_data.len() as u64);
            }

            let count = end - start;
            if count == 0 {
                // No predecessors: emit vbyte(0) and move on — skip copy/sort/dfn.
                vbyte::encode(0, &mut inb_data);
                start = end;
                if start >= next_free_at {
                    inb_flat.free_below(start);
                    next_free_at = start + (1 << 26);
                }
                if i == n / 2 {
                    crate::trace::probe("inbound Phase-4: midpoint (inb_flat+inb_data coexist)");
                }
                continue;
            }

            // Translate each predecessor NODE -> pre-order number (dfn);
            // drop unreachable predecessors (dfn == UNDEFINED). Use range_slice
            // for a zero-copy read when the range fits in one chunk; fall back
            // to copy_range otherwise. Store translated values in nb.
            let mut w = 0usize;
            if let Some(raw) = inb_flat.range_slice(start, end) {
                nb.clear();
                nb.reserve(raw.len());
                for &raw_val in raw {
                    let node = (raw_val & 0x7fff_ffff) as usize;
                    let pre = dfn[node];
                    if pre != u32::MAX {
                        nb.push(pre);
                        w += 1;
                    }
                }
            } else {
                inb_flat.copy_range(start, end, &mut nb);
                for r in 0..nb.len() {
                    let node = (nb[r] & 0x7fff_ffff) as usize;
                    let pre = dfn[node];
                    if pre != u32::MAX {
                        nb[w] = pre;
                        w += 1;
                    }
                }
            }

            // Sort by pre-order and dedup. Short-circuit for 0/1 translated
            // entries — the overwhelming majority of nodes in a heap graph have
            // at most one live predecessor, so this saves hundreds of millions of
            // sort calls.
            let unique_end = if w <= 1 {
                w
            } else {
                let pre_slice = &mut nb[..w];
                pre_slice.sort_unstable();
                let mut write = 1usize;
                for read in 1..pre_slice.len() {
                    if pre_slice[read] != pre_slice[write - 1] {
                        pre_slice[write] = pre_slice[read];
                        write += 1;
                    }
                }
                write
            };

            // Count-prefix makes each node's slice self-delimiting.
            vbyte::encode(unique_end as u32, &mut inb_data);
            // Delta-encode pre-order values.
            let mut prev: u32 = 0;
            for &pre in &nb[..unique_end] {
                vbyte::encode(pre - prev, &mut inb_data);
                prev = pre;
            }
            start = end;
            if start >= next_free_at {
                inb_flat.free_below(start);
                next_free_at = start + (1 << 26);
            }
            if i == n / 2 {
                crate::trace::probe("inbound Phase-4: midpoint (inb_flat+inb_data coexist)");
            }
        }
        drop(nb);
        drop(inb_flat);
        drop(in_cursors); // inbound CSR done; end-offset cursors no longer needed

        // Trailing sentinel = total byte length (bounds the last block's scan).
        inb_block_off.push(inb_data.len() as u64);

        if crate::trace::enabled() {
            eprintln!(
                "[trace-rss] inbound Phase-4: inb_data len={} MB cap={} MB block_off len={}",
                inb_data.len() / (1024 * 1024),
                inb_data.capacity() / (1024 * 1024),
                inb_block_off.len()
            );
        }
        crate::trace::probe("inbound Phase-4: after inb_data built");
        Ok((inb_block_off, inb_data))
    }

    /// Like `build()` (HPROF rescan path for inbound) but also drives a
    /// per-object callback with the dense-id outbound edge list, so MAT outbound
    /// can be emitted during the same HPROF scan.
    ///
    /// `on_outbound(dense_src, dense_dsts) -> io::Result<()>` receives each
    /// object's outbound edges as a `Vec<u32>` (allocated per object; the callback
    /// can drain/reuse it). Called in HPROF file order (not MAT id order).
    ///
    /// Memory benefit vs build_from_fwd: `fwd_targets` (6+ GB) can be dropped
    /// BEFORE calling this, so inb_flat and fwd_targets never coexist.
    /// `fwd_tgt_c` is also eliminated from the emit_outbound peak window.
    ///
    /// Cost: one extra HPROF scan (~3 min on the 34 GB dump).
    pub fn build_mat_scan<F>(
        self,
        dfn: &[u32],
        mut on_outbound: F,
    ) -> io::Result<(Vec<u64>, Vec<u8>)>
    where
        F: FnMut(usize, Vec<u32>) -> io::Result<()>,
    {
        let InboundBuilder {
            source,
            id_size,
            n,
            id_map,
            id_map_c,
            id_map_codec,
            class_addr_to_hist,
            field_plans_dense,
            mut in_cursors,
            total_inb,
            synthetic_edges,
            ..
        } = self;

        let id_map = match id_map {
            Some(m) => m,
            None => {
                let (blob, len) = id_map_c.expect("id_map neither live nor compressed");
                crate::id_map::IdMap::from_compressed(&blob, len, id_map_codec)?
            }
        };

        let mut inb_flat = crate::chunkvec::ChunkU32::zeroed(total_inb as usize);
        if crate::trace::enabled() {
            eprintln!(
                "[trace-rss] inbound mat-scan: total_inb={} edges, inb_flat={} MB",
                total_inb,
                (total_inb as usize * 4) / (1024 * 1024)
            );
        }
        crate::trace::probe("inbound mat-scan: after inb_flat alloc");

        // Single HPROF scan: fill inb_flat (inbound) AND collect per-object
        // forward edges for the on_outbound callback.
        {
            let mut r = source.open()?;
            let mut scratch: Vec<u8> = Vec::with_capacity(4096);
            let ids = id_size as u64;
            let mut cache = crate::id_map::IndexCache::new();

            loop {
                let tag = match r.u1() {
                    Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
                    other => other?,
                };
                let _ts = r.u4()?;
                let length = r.u4()? as u64;
                match tag {
                    tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                        Self::scan_inb_and_outbound(
                            &mut r,
                            id_size,
                            ids,
                            length,
                            &id_map,
                            &class_addr_to_hist,
                            &field_plans_dense,
                            &mut inb_flat,
                            &mut in_cursors,
                            &mut scratch,
                            &mut cache,
                            &mut on_outbound,
                        )?;
                    }
                    tags::HEAP_DUMP_END => break,
                    _ => {
                        r.skip(length)?;
                    }
                }
            }
        }

        drop(id_map);
        drop(class_addr_to_hist);
        drop(field_plans_dense);

        for &(src, dst) in &synthetic_edges {
            inb_flat.set(in_cursors[dst as usize] as usize, src);
            in_cursors[dst as usize] += 1;
        }

        crate::trace::probe("inbound: before Phase-4 (after mat-scan + drops)");
        Self::encode_phase4(n, total_inb, in_cursors, inb_flat, dfn)
    }

    /// Combined inbound-fill + outbound-collect for one HEAP_DUMP[_SEGMENT] record.
    /// Fills inb_flat and calls on_outbound for each object with its dense targets.
    #[allow(clippy::too_many_arguments)]
    fn scan_inb_and_outbound(
        r: &mut HprofReader,
        id_size: u8,
        ids: u64,
        mut remaining: u64,
        id_map: &crate::id_map::IdMap,
        class_addr_to_hist: &HashMap<u64, u32>,
        field_plans_dense: &[super::FieldPlan],
        inb_flat: &mut crate::chunkvec::ChunkU32,
        in_cursors: &mut Vec<u32>,
        scratch: &mut Vec<u8>,
        cache: &mut crate::id_map::IndexCache,
        on_outbound: &mut dyn FnMut(usize, Vec<u32>) -> io::Result<()>,
    ) -> io::Result<()> {
        use crate::types::heap;

        macro_rules! checked_sub {
            ($rem:expr, $sz:expr) => {
                $rem = $rem
                    .checked_sub($sz)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "segment overrun"))?;
            };
        }

        while remaining > 0 {
            let sub_tag = r.u1()?;
            checked_sub!(remaining, 1u64);

            match sub_tag {
                // Root sub-records: skip (no edges to/from these for our purposes)
                heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED => {
                    r.skip(ids)?;
                    checked_sub!(remaining, ids);
                }
                heap::ROOT_JNI_GLOBAL => {
                    r.skip(2 * ids)?;
                    checked_sub!(remaining, 2 * ids);
                }
                heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME => {
                    r.skip(ids + 8)?;
                    checked_sub!(remaining, ids + 8);
                }
                heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                    r.skip(ids + 4)?;
                    checked_sub!(remaining, ids + 4);
                }
                heap::ROOT_STICKY_CLASS | heap::ROOT_THREAD_OBJ => {
                    let skip = if sub_tag == heap::ROOT_THREAD_OBJ {
                        ids + 8
                    } else {
                        ids
                    };
                    r.skip(skip)?;
                    checked_sub!(remaining, skip);
                }
                heap::CLASS_DUMP => {
                    // Reuse fill_class_dump_edges logic via fill_heap_2b helper.
                    // For class objects we still need inbound edges (class→fields).
                    // Outbound from class dumps: skip for MAT outbound (MAT doesn't
                    // include static fields in outbound the same way).
                    let consumed = Pass2::fill_class_dump_edges(
                        r,
                        id_size,
                        id_map,
                        false, // do_fwd — don't collect for outbound
                        true,  // do_inb
                        &mut crate::chunkvec::ChunkU32::zeroed(0),
                        &mut Vec::new(),
                        &mut None,
                        inb_flat,
                        in_cursors,
                    )?;
                    checked_sub!(remaining, consumed);
                }
                heap::INSTANCE_DUMP => {
                    let addr = r.id()?;
                    r.skip(4)?; // stack_trace_serial
                    let class_id = r.id()?;
                    let data_len = r.u4()? as u64;
                    r.read_bytes_reuse(scratch, data_len as usize)?;
                    checked_sub!(remaining, ids + 4 + ids + 4 + data_len);

                    let src_idx = match id_map.index_of(addr) {
                        Some(i) => i,
                        None => continue,
                    };

                    let mut fwd: Vec<u32> = Vec::new();

                    // Edge: instance → class object (inbound + outbound)
                    if let Some(dst) = cache.index_of(id_map, class_id) {
                        // inbound
                        inb_flat.set(in_cursors[dst] as usize, src_idx as u32);
                        in_cursors[dst] += 1;
                        fwd.push(dst as u32);
                    }

                    // Object-type instance fields
                    if let Some(&cidx) = class_addr_to_hist.get(&class_id) {
                        for &(off, _excluded) in &field_plans_dense[cidx as usize] {
                            let off = off as usize;
                            if off + id_size as usize <= scratch.len() {
                                let ref_val = super::read_ref(&scratch[off..], id_size as usize);
                                if ref_val != 0 {
                                    if let Some(dst) = cache.index_of(id_map, ref_val) {
                                        inb_flat.set(in_cursors[dst] as usize, src_idx as u32);
                                        in_cursors[dst] += 1;
                                        fwd.push(dst as u32);
                                    }
                                }
                            }
                        }
                    }

                    on_outbound(src_idx, fwd)?;
                }
                heap::OBJ_ARRAY_DUMP => {
                    let addr = r.id()?;
                    r.skip(4)?; // stack_trace_serial
                    let count = r.u4()? as u64;
                    let _elem_class_id = r.id()?;
                    let byte_len = count.saturating_mul(ids);
                    r.read_bytes_reuse(scratch, byte_len as usize)?;
                    checked_sub!(remaining, ids + 4 + 4 + ids + byte_len);

                    let src_idx = match id_map.index_of(addr) {
                        Some(i) => i,
                        None => continue,
                    };

                    let mut fwd: Vec<u32> = Vec::new();
                    for i in 0..count as usize {
                        let off = i * id_size as usize;
                        if off + id_size as usize <= scratch.len() {
                            let ref_val = super::read_ref(&scratch[off..], id_size as usize);
                            if ref_val != 0 {
                                if let Some(dst) = cache.index_of(id_map, ref_val) {
                                    inb_flat.set(in_cursors[dst] as usize, src_idx as u32);
                                    in_cursors[dst] += 1;
                                    fwd.push(dst as u32);
                                }
                            }
                        }
                    }
                    on_outbound(src_idx, fwd)?;
                }
                heap::PRIM_ARRAY_DUMP => {
                    // No reference edges in primitive arrays.
                    let addr = r.id()?;
                    r.skip(4)?;
                    let count = r.u4()? as u64;
                    let elem_type = r.u1()?;
                    let elem_size = crate::types::HprofType::from_code(elem_type)
                        .map(|t| t.byte_size() as u64)
                        .unwrap_or(1);
                    let byte_len = count.saturating_mul(elem_size);
                    r.skip(byte_len)?;
                    checked_sub!(remaining, ids + 4 + 4 + 1 + byte_len);
                    if let Some(src_idx) = id_map.index_of(addr) {
                        on_outbound(src_idx, Vec::new())?;
                    }
                }
                heap::HEAP_DUMP_INFO => {
                    r.skip(4 + ids)?;
                    checked_sub!(remaining, 4 + ids);
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unknown heap sub-tag {sub_tag:#x}"),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Scatter-fill `fwd_tgt` by scanning the HPROF file. Uses `fwd_off` as
/// per-object write cursors (modified in-place): on entry `fwd_off[d]` is the
/// start of object d's outbound range; on exit `fwd_off[d]` is the end of that
/// range (= `fwd_off_original[d+1]`). Caller can therefore reconstruct each
/// object's range as `fwd_off[d-1] .. fwd_off[d]` (with `fwd_off[-1] = 0`).
///
/// Memory: only O(1) transient state beyond the caller-supplied Vecs.
pub fn rescan_outbound(
    ctx: &MatOutboundRescanCtx,
    fwd_off: &mut Vec<u32>,
    fwd_tgt: &mut Vec<u32>,
) -> io::Result<()> {
    use crate::types::tags;
    use std::io::ErrorKind;

    // Restore id_map from compressed blob (or live copy).
    let id_map = match ctx.id_map_c.as_ref() {
        Some((blob, len)) => crate::id_map::IdMap::from_compressed(blob, *len, ctx.id_map_codec)?,
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "rescan_outbound: id_map not compressed (unexpected)",
            ));
        }
    };
    let ids = ctx.id_size as u64;
    let mut scratch: Vec<u8> = Vec::with_capacity(4096);
    let mut cache = crate::id_map::IndexCache::new();

    let mut r = ctx.source.open()?;
    loop {
        let tag = match r.u1() {
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            other => other?,
        };
        let _ts = r.u4()?;
        let length = r.u4()? as u64;
        match tag {
            tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                scan_fwd_segment(
                    &mut r,
                    ctx.id_size,
                    ids,
                    length,
                    &id_map,
                    &ctx.class_addr_to_hist,
                    &ctx.field_plans_dense,
                    fwd_off,
                    fwd_tgt,
                    &mut scratch,
                    &mut cache,
                )?;
            }
            tags::HEAP_DUMP_END => break,
            _ => {
                r.skip(length)?;
            }
        }
    }
    Ok(())
}

/// Skip one CLASS_DUMP sub-record, returning the number of bytes consumed.
fn skip_class_dump(r: &mut HprofReader, id_size: u8, ids: u64) -> io::Result<u64> {
    use super::scan::value_size;
    let mut consumed = 0u64;
    // class_addr (id) + stack_trace_serial (u32) + super_id (id) + loader_id (id)
    //   + signers_id + domain_id + reserved1 + reserved2 (4×ids) + instance_size (u32)
    r.skip(ids + 4 + ids + ids + ids * 4 + 4)?;
    consumed += ids + 4 + ids + ids + ids * 4 + 4;
    // constant pool
    let cp = r.u2()? as u64;
    consumed += 2;
    for _ in 0..cp {
        r.skip(2)?;
        consumed += 2; // cp_index
        let tp = r.u1()?;
        consumed += 1;
        let vs = value_size(tp, id_size);
        r.skip(vs)?;
        consumed += vs;
    }
    // static fields
    let sc = r.u2()? as u64;
    consumed += 2;
    for _ in 0..sc {
        r.skip(ids)?;
        consumed += ids; // name_id
        let tp = r.u1()?;
        consumed += 1;
        let vs = value_size(tp, id_size);
        r.skip(vs)?;
        consumed += vs;
    }
    // instance fields (just descriptors, no values)
    let ic = r.u2()? as u64;
    consumed += 2;
    r.skip(ic * (ids + 1))?;
    consumed += ic * (ids + 1);
    Ok(consumed)
}

/// Scatter outbound edges from one HEAP_DUMP[_SEGMENT] record into `fwd_tgt`,
/// using `fwd_off[d]` as per-object write cursor (incremented in-place).
#[allow(clippy::too_many_arguments)]
fn scan_fwd_segment(
    r: &mut HprofReader,
    id_size: u8,
    ids: u64,
    mut remaining: u64,
    id_map: &crate::id_map::IdMap,
    class_addr_to_hist: &HashMap<u64, u32>,
    field_plans_dense: &[super::FieldPlan],
    fwd_off: &mut Vec<u32>,
    fwd_tgt: &mut Vec<u32>,
    scratch: &mut Vec<u8>,
    cache: &mut crate::id_map::IndexCache,
) -> io::Result<()> {
    use crate::types::heap;

    macro_rules! checked_sub {
        ($rem:expr, $sz:expr) => {
            $rem = $rem
                .checked_sub($sz)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "segment overrun"))?;
        };
    }

    while remaining > 0 {
        let sub_tag = r.u1()?;
        checked_sub!(remaining, 1u64);

        match sub_tag {
            heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED => {
                r.skip(ids)?;
                checked_sub!(remaining, ids);
            }
            heap::ROOT_JNI_GLOBAL => {
                r.skip(2 * ids)?;
                checked_sub!(remaining, 2 * ids);
            }
            heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME => {
                r.skip(ids + 8)?;
                checked_sub!(remaining, ids + 8);
            }
            heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                r.skip(ids + 4)?;
                checked_sub!(remaining, ids + 4);
            }
            heap::ROOT_STICKY_CLASS | heap::ROOT_THREAD_OBJ => {
                let skip = if sub_tag == heap::ROOT_THREAD_OBJ {
                    ids + 8
                } else {
                    ids
                };
                r.skip(skip)?;
                checked_sub!(remaining, skip);
            }
            heap::CLASS_DUMP => {
                // Class dumps don't contribute outbound edges in the MAT model.
                // Skip by parsing the variable-length structure.
                let consumed = skip_class_dump(r, id_size, ids)?;
                checked_sub!(remaining, consumed);
            }
            heap::INSTANCE_DUMP => {
                let addr = r.id()?;
                r.skip(4)?;
                let class_id = r.id()?;
                let data_len = r.u4()? as u64;
                r.read_bytes_reuse(scratch, data_len as usize)?;
                checked_sub!(remaining, ids + 4 + ids + 4 + data_len);

                let src_idx = match id_map.index_of(addr) {
                    Some(i) => i,
                    None => continue,
                };

                // Edge: instance → class object
                if let Some(dst) = cache.index_of(id_map, class_id) {
                    scatter_edge(fwd_off, fwd_tgt, src_idx, dst);
                }

                // Object-type instance fields
                if let Some(&cidx) = class_addr_to_hist.get(&class_id) {
                    for &(off, _excluded) in &field_plans_dense[cidx as usize] {
                        let off = off as usize;
                        if off + id_size as usize <= scratch.len() {
                            let ref_val = super::read_ref(&scratch[off..], id_size as usize);
                            if ref_val != 0 {
                                if let Some(dst) = cache.index_of(id_map, ref_val) {
                                    scatter_edge(fwd_off, fwd_tgt, src_idx, dst);
                                }
                            }
                        }
                    }
                }
            }
            heap::OBJ_ARRAY_DUMP => {
                let addr = r.id()?;
                r.skip(4)?;
                let count = r.u4()? as u64;
                let _elem_class_id = r.id()?;
                let byte_len = count.saturating_mul(ids);
                r.read_bytes_reuse(scratch, byte_len as usize)?;
                checked_sub!(remaining, ids + 4 + 4 + ids + byte_len);

                let src_idx = match id_map.index_of(addr) {
                    Some(i) => i,
                    None => continue,
                };

                for i in 0..count as usize {
                    let off = i * id_size as usize;
                    if off + id_size as usize <= scratch.len() {
                        let ref_val = super::read_ref(&scratch[off..], id_size as usize);
                        if ref_val != 0 {
                            if let Some(dst) = cache.index_of(id_map, ref_val) {
                                scatter_edge(fwd_off, fwd_tgt, src_idx, dst);
                            }
                        }
                    }
                }
            }
            heap::PRIM_ARRAY_DUMP => {
                let addr = r.id()?;
                r.skip(4)?;
                let count = r.u4()? as u64;
                let elem_type = r.u1()?;
                let elem_size = crate::types::HprofType::from_code(elem_type)
                    .map(|t| t.byte_size() as u64)
                    .unwrap_or(1);
                r.skip(count.saturating_mul(elem_size))?;
                checked_sub!(remaining, ids + 4 + 4 + 1 + count.saturating_mul(elem_size));
                let _ = addr; // no outbound from prim arrays
            }
            heap::HEAP_DUMP_INFO => {
                r.skip(4 + ids)?;
                checked_sub!(remaining, 4 + ids);
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown heap sub-tag {sub_tag:#x}"),
                ));
            }
        }
    }
    Ok(())
}

/// Scatter one edge `(src_idx → dst)` into `fwd_tgt` using `fwd_off[src_idx]`
/// as the write cursor (incremented in-place after writing).
#[inline]
fn scatter_edge(fwd_off: &mut Vec<u32>, fwd_tgt: &mut Vec<u32>, src_idx: usize, dst: usize) {
    let pos = fwd_off[src_idx] as usize;
    if pos < fwd_tgt.len() {
        fwd_tgt[pos] = dst as u32;
        fwd_off[src_idx] += 1;
    }
}
