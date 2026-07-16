//! Pass-2 data model: graph output struct + inbound-builder state.

use std::{
    collections::HashMap,
    io::{self, ErrorKind},
};

use crate::{reader::HprofReader, types::tags, vbyte};

use super::{FieldPlan, Pass2};

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
/// though they usually hold separate backing arrays. Opt-in via `--dup-strings`.
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
    /// len desc then text asc. Only populated with `--dup-strings`.
    #[serde(default)]
    pub top_by_length: Vec<DupStringSample>,
    /// Wasted space in char[]/byte[] arrays backing Strings. `None` unless
    /// `--dup-strings` computed it.
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
    /// `fwd_offsets`. The largest single array pass2 builds (~6GB on big dumps).
    pub fwd_targets: Vec<u32>,
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
    /// opt-in `--dup-strings` flag is set; `None` otherwise (zero extra work,
    /// zero RSS on the default path). See [`DupStrings`].
    pub dup_strings: Option<DupStrings>,
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
    /// Raw container-attribution records from field-decode under `--collections`;
    /// `None` when the flag was off. Consumed in build_model to attach retained
    /// sizes and aggregate. Not serialized.
    pub collection_attribution_raw: Option<Vec<AttributionRaw>>,
    /// True when the holder-edge or container-record cap was hit (attribution
    /// data is a bounded sample). Not serialized.
    pub collection_attribution_truncated: bool,
    /// Sum of `capacity` fields across all live `java/nio/DirectByteBuffer`
    /// instances. 0 when no such instances are found or the field cannot be
    /// resolved. Computed unconditionally during the pass2 field-decode scan.
    #[allow(dead_code)]
    pub direct_byte_buffer_capacity_sum: u64,
}

/// Deferred inbound-CSR construction. Built by `Pass2::build` with everything
/// needed to run the inbound scan + delta-encode later (after rpo frees its
/// arrays), keeping the ~5.5GB inbound CSR off the rpo-phase RSS peak.
pub struct InboundBuilder {
    pub(crate) path: String,
    pub(crate) id_size: u8,
    pub(crate) ref_size: usize,
    pub(crate) n: usize,
    /// Live id_map as constructed by `build`; taken by `compress_id_map`.
    pub(crate) id_map: Option<crate::id_map::IdMap>,
    /// Compressed id_map (blob, element_count); set by `compress_id_map`.
    pub(crate) id_map_c: Option<(Vec<u8>, usize)>,
    pub(crate) id_map_codec: crate::cvec::Codec,
    pub(crate) class_addrs: std::collections::HashSet<u64>,
    pub(crate) field_plans: HashMap<u64, FieldPlan>,
    /// Prefix-summed inbound start cursors (in_degree after prefix-sum), len n.
    pub(crate) in_cursors: Vec<u32>,
    pub(crate) total_inb: u64,
    /// Synthetic thread->local edges (src,dst), already deduped.
    pub(crate) synthetic_edges: Vec<(u32, u32)>,
}

impl InboundBuilder {
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

    /// Run the inbound scan + Phase-4 encode. Returns (inb_offsets, inb_data).
    pub fn build(self, dfn: &[u32]) -> io::Result<(Vec<u64>, Vec<u8>)> {
        let InboundBuilder {
            path,
            id_size,
            ref_size,
            n,
            id_map,
            id_map_c,
            id_map_codec,
            class_addrs,
            field_plans,
            mut in_cursors,
            total_inb,
            synthetic_edges,
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
            let mut r = HprofReader::open(&path)?;
            let mut scratch: Vec<u8> = Vec::with_capacity(4096);
            let mut fwd_t_stub: Vec<u32> = Vec::new();
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
                            ref_size,
                            length,
                            &id_map,
                            &class_addrs,
                            &field_plans,
                            false,
                            true,
                            &mut fwd_t_stub,
                            &mut fwd_offsets_stub,
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

        // id_map / class_addrs / field_plans are consumed only by the 2b scan
        // above. Free them now (id_map alone is ~4.1 GB at 514M objects) before
        // the Phase-4 encode allocates inb_data, trimming the global RSS peak.
        drop(id_map);
        drop(class_addrs);
        drop(field_plans);

        // Synthetic thread->local INBOUND edges.
        for &(src, dst) in &synthetic_edges {
            inb_flat.set(in_cursors[dst as usize] as usize, src);
            in_cursors[dst as usize] += 1;
        }

        crate::trace::probe("inbound: before Phase-4 (after 2b scan + drops)");
        // -- Phase 4: Build inbound CSR (blocked offsets + count-prefixed data) --
        // inb_block_off[b] = byte offset where node (b*INB_BLOCK)'s slice begins.
        // Each node's slice = vbyte(count) then `count` vbyte pre-order deltas.
        let mut inb_block_off: Vec<u64> = Vec::with_capacity(n / INB_BLOCK + 2);
        let mut inb_data: Vec<u8> = Vec::new();
        // CSR is contiguous: start[i] = end of node i-1 = in_cursors[i-1] after fill.
        let mut start = 0usize;
        // Reusable per-node scratch: copy each node's inbound slice out of the
        // chunked store so we can sort/dedup it, then free chunks behind us.
        let mut nb: Vec<u32> = Vec::new();
        // Free consumed chunks every ~256 M slots crossed (one chunk).
        let mut next_free_at: usize = 1 << 26;
        for i in 0..n {
            let end = in_cursors[i] as usize; // in_cursors[i] = end offset after fill
            inb_flat.copy_range(start, end, &mut nb);
            // Translate each stripped predecessor NODE -> its pre-order number
            // (dfn); drop unreachable predecessors (dfn == UNDEFINED). Storing
            // pre-order values here means dominator Phase 1 never needs dfn, so
            // the caller frees dfn (2GB) before the Phase-1 peak. Reuse `nb` in
            // place: overwrite the front with translated pre-order values.
            let mut w = 0usize;
            for r in 0..nb.len() {
                let node = (nb[r] & 0x7fff_ffff) as usize;
                let pre = dfn[node];
                if pre != u32::MAX {
                    nb[w] = pre;
                    w += 1;
                }
            }
            // Sort by pre-order and dedup (two distinct nodes cannot share a
            // pre-order, so this preserves the node-level dedup done at fill).
            let pre_slice = &mut nb[..w];
            pre_slice.sort_unstable();
            let unique_end = {
                if pre_slice.is_empty() {
                    0
                } else {
                    let mut write = 1usize;
                    for read in 1..pre_slice.len() {
                        if pre_slice[read] != pre_slice[write - 1] {
                            pre_slice[write] = pre_slice[read];
                            write += 1;
                        }
                    }
                    write
                }
            };
            // Record a sampled block offset at each block boundary (BEFORE the
            // count-prefix), so a lookup for any node in the block can seek here
            // and scan-skip forward to the target node.
            if i % INB_BLOCK == 0 {
                inb_block_off.push(inb_data.len() as u64);
            }
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
}
