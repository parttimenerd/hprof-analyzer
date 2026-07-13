use std::{
    collections::HashMap,
    io::{self, ErrorKind},
};

use crate::{
    pass1::{ClassInfo, Pass1},
    reader::HprofReader,
    types::{HprofType, heap, tags},
    vbyte,
};

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

pub struct Graph {
    pub n: usize,
    pub format: String,
    pub file_size: u64,
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
    pub gc_root_indices: Vec<u32>,
    /// Per-root HPROF sub-tag, aligned 1:1 with `gc_root_indices` (same order).
    /// A representative type when an index has multiple root records (the
    /// minimum sub-tag, deterministically). `heap::ROOT_SYSTEM_CLASS` (0x00)
    /// marks synthetic system-class roots. Powers `gc_roots_by_type` (B1) and
    /// the default why-alive line, so it is carried unconditionally.
    #[allow(dead_code)]
    pub gc_root_types: Vec<u8>,
    pub shallow: Vec<u32>,
    pub class_idx: Vec<u32>,
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
    /// Decoded `java.lang.Thread.name` per HPROF thread serial. Populated by a
    /// bounded multi-pass worklist in `Pass2::build` (thread objects → their
    /// name String → the String's char/byte array → decoded text). Bounded by
    /// the number of threads (hundreds), so it never touches the per-object RSS
    /// budget. Absent serials render as an unnamed thread.
    pub thread_names: std::collections::HashMap<u32, String>,
    /// Per-thread count of GC-thread-local roots that resolved to a live object
    /// (thread_serial -> #resolved locals). Filled from `p1.thread_local_pairs`
    /// during synthetic-edge resolution, using the SAME guard (thread/local both
    /// resolve to indices and are distinct). Bounded by #threads (hundreds), so
    /// it never touches the per-object RSS budget on multi-GB dumps.
    pub thread_local_counts: std::collections::HashMap<u32, u64>,
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
    pub class_obj_class_idx: HashMap<u32, u32>, // class-obj index -> class-histogram row (sparse; absent = not a class obj)
    // Forward CSR
    pub fwd_offsets: Vec<u32>,
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
    // Filled by later passes
    pub idom: Vec<u32>,
    pub retained: Vec<u64>,
    pub has_same_class_ancestor: crate::bitset::Bitset,
}

/// Deferred inbound-CSR construction. Built by `Pass2::build` with everything
/// needed to run the inbound scan + delta-encode later (after rpo frees its
/// arrays), keeping the ~5.5GB inbound CSR off the rpo-phase RSS peak.
pub struct InboundBuilder {
    path: String,
    id_size: u8,
    ref_size: usize,
    n: usize,
    /// Live id_map as constructed by `build`; taken by `compress_id_map`.
    id_map: Option<crate::id_map::IdMap>,
    /// Compressed id_map (blob, element_count); set by `compress_id_map`.
    id_map_c: Option<(Vec<u8>, usize)>,
    id_map_codec: crate::cvec::Codec,
    class_addrs: std::collections::HashSet<u64>,
    field_plans: HashMap<u64, FieldPlan>,
    /// Prefix-summed inbound start cursors (in_degree after prefix-sum), len n.
    in_cursors: Vec<u32>,
    total_inb: u64,
    /// Synthetic thread->local edges (src,dst), already deduped.
    synthetic_edges: Vec<(u32, u32)>,
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

// ── Size helpers ───────────────────────────────────────────────────────────

fn align_up(n: usize, align: usize) -> usize {
    n.div_ceil(align) * align
}

/// Byte sizes of non-Object (primitive) fields for a class's own fields only.
fn own_prim_bytes(ci: &ClassInfo, _ref_size: usize) -> usize {
    ci.fields
        .iter()
        .filter(|(_, t)| *t != HprofType::Object)
        .map(|(_, t)| t.byte_size())
        .sum()
}

fn own_obj_count(ci: &ClassInfo) -> usize {
    ci.fields
        .iter()
        .filter(|(_, t)| *t == HprofType::Object)
        .count()
}

/// Recursively compute unaligned instance body size (MAT formula).
fn calculate_size_recursive(
    class_addr: u64,
    class_map: &HashMap<u64, ClassInfo>,
    ptr_size: usize,
    ref_size: usize,
    cache: &mut HashMap<u64, usize>,
) -> usize {
    if let Some(&cached) = cache.get(&class_addr) {
        return cached;
    }
    let result = match class_map.get(&class_addr) {
        None => ptr_size + ref_size, // unknown class, use minimum
        Some(ci) => {
            if ci.super_id == 0 {
                ptr_size + ref_size
            } else {
                let own = own_obj_count(ci) * ref_size + own_prim_bytes(ci, ref_size);
                let super_size =
                    calculate_size_recursive(ci.super_id, class_map, ptr_size, ref_size, cache);
                align_up(own + super_size, ref_size)
            }
        }
    };
    cache.insert(class_addr, result);
    result
}

fn instance_shallow_size(
    class_addr: u64,
    class_map: &HashMap<u64, ClassInfo>,
    ptr_size: usize,
    ref_size: usize,
    cache: &mut HashMap<u64, usize>,
) -> u32 {
    let inner = calculate_size_recursive(class_addr, class_map, ptr_size, ref_size, cache);
    align_up(inner, 8) as u32
}

fn obj_array_shallow(num_elem: u64, ptr_size: usize, ref_size: usize) -> u32 {
    align_up(ptr_size + ref_size + 4 + num_elem as usize * ref_size, 8) as u32
}

fn prim_array_shallow(num_elem: u64, elem_size: usize, ptr_size: usize, ref_size: usize) -> u32 {
    let header = align_up(ptr_size + ref_size + 4, ref_size);
    align_up(header + num_elem as usize * elem_size, 8) as u32
}

fn class_obj_shallow(ci: &ClassInfo, _ptr_size: usize, ref_size: usize) -> u32 {
    // MAT parity: class-object shallow = alignUp(staticObjFields*refSize + staticPrimBytes, 8).
    // No pointer+ref floor (matClassSize in hprof-redact); classes with no statics get 0.
    let computed = ci.static_obj_count as usize * ref_size + ci.static_prim_bytes as usize;
    align_up(computed, 8) as u32
}

// ── Field layout cache ─────────────────────────────────────────────────────

/// Per-class instance-field plan: byte offset of each Object-type field within
/// the INSTANCE_DUMP data, paired with whether that edge is excluded from the
/// dominator computation (weak-reference / finalizer fields).
pub type FieldPlan = Vec<(u32, bool)>;

/// Build the FieldPlan for every class in `class_map`, walking each class's
/// super chain once. Excluded fields are marked via `is_excluded_field`.
/// Precomputing this up front lets the hot scan loop borrow immutably with no
/// per-instance allocation.
fn build_field_plans(
    class_map: &HashMap<u64, ClassInfo>,
    strings: &HashMap<u64, String>,
    id_size: usize,
) -> HashMap<u64, FieldPlan> {
    let mut plans: HashMap<u64, FieldPlan> = HashMap::with_capacity(class_map.len());
    let mut chain: Vec<u64> = Vec::new();
    for &class_addr in class_map.keys() {
        chain.clear();
        let mut cur = class_addr;
        loop {
            match class_map.get(&cur) {
                None => break,
                Some(ci) => {
                    chain.push(cur);
                    if ci.super_id == 0 {
                        break;
                    }
                    cur = ci.super_id;
                }
            }
        }
        let mut plan: FieldPlan = Vec::new();
        let mut byte_offset = 0usize;
        for &caddr in &chain {
            let ci = match class_map.get(&caddr) {
                Some(c) => c,
                None => break,
            };
            let cname = strings.get(&ci.name_id).map(|s| s.as_str()).unwrap_or("");
            for &(fname_id, t) in &ci.fields {
                let fsize = if t == HprofType::Object {
                    id_size
                } else {
                    t.byte_size()
                };
                if t == HprofType::Object {
                    let fname = strings.get(&fname_id).map(|s| s.as_str()).unwrap_or("");
                    let excluded = is_excluded_field(cname, fname);
                    plan.push((byte_offset as u32, excluded));
                }
                byte_offset += fsize;
            }
        }
        plans.insert(class_addr, plan);
    }
    plans
}

// ── Excluded field detection ───────────────────────────────────────────────

/// Returns true if (class_name, field_name) is an excluded reference edge.
fn is_excluded_field(class_name: &str, field_name: &str) -> bool {
    matches!(
        (class_name, field_name),
        ("java/lang/ref/Reference", "referent")
            | ("java/lang/ref/Finalizer", "unfinalized")
            | ("java/lang/Runtime", "<Unfinalized>")
    )
}

// ── Compressed OOPs detection ──────────────────────────────────────────────

/// After scanning all OBJ_ARRAY addresses with element counts, detect if
/// ref_size should be 4 (compressed OOPs). Only relevant for id_size==8.
fn detect_ref_size(id_size: u8, array_addr_counts: &[(u64, u64)]) -> u8 {
    if id_size != 8 {
        return id_size;
    }
    // Sort by address
    let mut sorted: Vec<(u64, u64)> = array_addr_counts.to_vec();
    sorted.sort_unstable_by_key(|&(a, _)| a);
    let mut prev_start = 0u64;
    let mut prev_uncomp_end = 0u64;
    for &(addr, count) in &sorted {
        if prev_uncomp_end > 0 && addr > prev_start && addr < prev_uncomp_end {
            return 4;
        }
        prev_start = addr;
        // header (16) + elements*8 for uncompressed
        prev_uncomp_end = addr + 16 + count * 8;
    }
    id_size
}

// ── Class name building ────────────────────────────────────────────────────

fn prim_array_class_name(elem_type_code: u8) -> &'static str {
    match elem_type_code {
        4 => "[Z",  // boolean
        5 => "[C",  // char
        6 => "[F",  // float
        7 => "[D",  // double
        8 => "[B",  // byte
        9 => "[S",  // short
        10 => "[I", // int
        11 => "[J", // long
        _ => "[?",
    }
}

/// True iff `name` is a JVM primitive-array class descriptor: a single `[`
/// followed by exactly one primitive type char (`Z C F D S I J B`), length 2.
/// Object-array (`[Ljava/lang/String;`) and multi-dim (`[[I`) names are false.
fn is_primitive_array_class_name(name: &str) -> bool {
    name.len() == 2
        && name.as_bytes()[0] == b'['
        && matches!(
            name.as_bytes()[1],
            b'Z' | b'C' | b'F' | b'D' | b'S' | b'I' | b'J' | b'B'
        )
}

/// Decide whether a boot-loader (loader_id==0) class object should be added as
/// a synthetic SYSTEM_CLASS GC root, mirroring MAT's
/// `HprofParserHandlerImpl.fillIn` `addSystemClassRootsIfMissing` behaviour.
///
/// MAT (fillIn, lines 679-699) only runs its class-rooting loop when NO
/// system-class (sticky) roots were found in the dump; that loop roots
/// boot-loader classes that are **not array types** and not already roots.
/// When sticky-class roots ARE present (`has_sticky` == true, the normal HPROF
/// case), MAT roots NOTHING here — boot-loader classes are reached via the real
/// sticky roots + structural edges. Rooting non-array boot classes
/// unconditionally over-marks objects MAT discards as unreachable garbage
/// (the big-dump +4,645-object / +452-class frontier divergence).
///
/// The one deliberate deviation: instance-less **primitive-array** class
/// objects (`[Z [C [F [D [S [I [J [B`) are ALWAYS rooted. MAT's dominator tree
/// root-attaches those metadata objects even without an explicit GC root; this
/// is the empirically-verified "Group B" mirror needed for small-dump parity,
/// and it has no effect on dumps that already reach those classes via live
/// instances.
fn should_add_system_class_root(is_array: bool, is_prim_array: bool, has_sticky: bool) -> bool {
    if is_prim_array {
        // Group B: always root the instance-less primitive-array metadata objects.
        return true;
    }
    if is_array {
        // Object arrays / multi-dim arrays: never synthetically rooted — MAT's
        // fillIn guard is `!clazz.isArrayType()`.
        return false;
    }
    // Non-array boot-loader class: root it only when MAT would, i.e. only when
    // the dump has no sticky (SYSTEM_CLASS) roots of its own.
    !has_sticky
}

/// Resolve pass1's STACK_TRACE/STACK_FRAME tables into pre-rendered thread
/// stacks. Each frame becomes `class.method (source:line)`; unresolved string
/// ids fall back to their hex id, unknown/negative line numbers are rendered
/// per HPROF convention. Traces with no frames are dropped. Output is sorted by
/// `thread_serial` for determinism. Small (one entry per thread trace).
fn build_thread_stacks(p1: &Pass1) -> Vec<ThreadStack> {
    let resolve = |id: u64| -> Option<&str> { p1.strings.get(&id).map(|s| s.as_str()) };
    let class_name_of = |serial: u32| -> Option<&str> {
        let addr = *p1.class_serial_to_addr.get(&serial)?;
        let ci = p1.class_map.get(&addr)?;
        p1.strings.get(&ci.name_id).map(|s| s.as_str())
    };

    let mut out: Vec<ThreadStack> = Vec::new();
    for (&stack_serial, frame_ids) in p1.stack_traces.iter() {
        if frame_ids.is_empty() {
            continue;
        }
        let thread_serial = p1
            .stack_trace_thread
            .get(&stack_serial)
            .copied()
            .unwrap_or(0);
        let thread_obj_idx = p1
            .thread_serial_to_obj_id
            .get(&thread_serial)
            .and_then(|&addr| p1.id_map.index_of(addr))
            .map(|i| i as u32)
            .unwrap_or(u32::MAX);

        let mut frames = Vec::with_capacity(frame_ids.len());
        for &fid in frame_ids {
            let Some(f) = p1.stack_frames.get(&fid) else {
                frames.push(format!("<unknown frame {fid:#x}>"));
                continue;
            };
            let class = class_name_of(f.class_serial).map(pretty_binary_name);
            let method = resolve(f.method_name_id);
            let source = resolve(f.source_file_id);
            frames.push(render_frame(
                class.as_deref(),
                method,
                source,
                f.class_serial,
                f.line_number,
            ));
        }
        out.push(ThreadStack {
            thread_serial,
            thread_obj_idx,
            frames,
        });
    }
    out.sort_by_key(|t| t.thread_serial);
    out
}

/// Compute the absolute byte offset of one named instance field within an
/// object's INSTANCE_DUMP blob. HotSpot lays out SUPERCLASS fields first, so we
/// walk the super-chain child→parent, then sum field widths oldest-ancestor
/// first (the REVERSE of the collected chain). Returns `(offset, type)` for the
/// first field whose name matches `field_name` AND whose DECLARING class name
/// matches `owner_class`, or `None` if absent. `ref_size` widths are used for
/// Object fields so offsets match the on-disk blob (compressed OOPs).
///
/// The `owner_class` filter is essential: a subclass may declare its own field
/// with the same simple name (e.g. a Scala `PhilosopherThread.name`) that would
/// otherwise be picked instead of the inherited `java.lang.Thread.name`.
fn field_offset(
    class_addr: u64,
    field_name: &str,
    owner_class: &str,
    class_map: &HashMap<u64, ClassInfo>,
    strings: &HashMap<u64, String>,
    obj_ref_width: usize,
) -> Option<(u32, HprofType)> {
    // Collect the super-chain child-first.
    let mut chain: Vec<u64> = Vec::new();
    let mut cur = class_addr;
    loop {
        match class_map.get(&cur) {
            None => break,
            Some(ci) => {
                chain.push(cur);
                if ci.super_id == 0 {
                    break;
                }
                cur = ci.super_id;
            }
        }
    }
    // HPROF stores instance field VALUES subclass-first: the object's own class
    // fields come first in the blob, then the immediate superclass's, and so on
    // up the chain (see `ClassInfo.fields` doc in pass1). Accumulate widths in
    // that same child-first order — i.e. walk `chain` as collected, NOT reversed.
    // Object references inside an INSTANCE_DUMP blob are always `id_size` wide
    // (the compressed-oops narrowing only applies to object-array elements), so
    // callers pass `id_size` as `obj_ref_width`.
    let mut byte_offset = 0usize;
    for &caddr in chain.iter() {
        let ci = class_map.get(&caddr)?;
        let cname = strings.get(&ci.name_id).map(|s| s.as_str()).unwrap_or("");
        let owner_matches = cname == owner_class;
        for &(fname_id, t) in &ci.fields {
            let fsize = if t == HprofType::Object {
                obj_ref_width
            } else {
                t.byte_size()
            };
            let fname = strings.get(&fname_id).map(|s| s.as_str()).unwrap_or("");
            if owner_matches && fname == field_name {
                return Some((byte_offset as u32, t));
            }
            byte_offset += fsize;
        }
    }
    None
}

/// Decode each thread's `java.lang.Thread.name` String into UTF-8 via a bounded
/// multi-pass worklist. All captured sets are bounded by the number of threads
/// (hundreds) and the tiny Strings/arrays they reference, so this stays off the
/// per-object RSS budget even on multi-GB dumps.
///
/// Runs THREE extra full-file sequential scans (the reader is streaming-only, so
/// each multi-hop forward reference needs its own pass over the file):
///   A: Thread object → its `name` String address.
///   B: String → its backing array address + `coder` byte (Java 8 has no coder).
///   C: backing PRIM_ARRAY → its raw element bytes.
/// Then chains the maps per serial and decodes. Passes are NOT merged because a
/// hop's target addresses are only known after the previous pass completes.
///
/// Field offsets are derived from each object's ACTUAL class id (memoized),
/// because a heap may hold several loader-distinct class objects named
/// `java/lang/Thread` / `java/lang/String`, and thread objects are frequently
/// subclasses whose inherited `name` sits past the subclass's own fields.
fn resolve_thread_names(path: &str, p1: &Pass1) -> io::Result<HashMap<u32, String>> {
    let mut names: HashMap<u32, String> = HashMap::new();
    if p1.thread_serial_to_obj_id.is_empty() {
        return Ok(names);
    }
    let id_size = p1.id_size;
    // Object references inside an INSTANCE_DUMP blob are always id_size wide (the
    // compressed-oops narrowing detected for array elements does not apply here).
    let obj_ref_width = id_size as usize;
    let class_map = &p1.class_map;
    let strings = &p1.strings;

    // ── Pass A: Thread object addr → name String addr ────────────────────────
    // Bounded by #threads. The `name` offset is resolved from each thread's own
    // class id (memoized), matching only the field declared by java/lang/Thread
    // so a subclass field of the same simple name cannot shadow it.
    let wanted_threads: std::collections::HashSet<u64> =
        p1.thread_serial_to_obj_id.values().copied().collect();
    let mut thread_to_name_addr: HashMap<u64, u64> = HashMap::new();
    let mut name_off_cache: HashMap<u64, Option<usize>> = HashMap::new();
    scan_instance_blobs(path, id_size, &wanted_threads, |addr, class_id, blob| {
        let name_off = *name_off_cache.entry(class_id).or_insert_with(|| {
            match field_offset(
                class_id,
                "name",
                "java/lang/Thread",
                class_map,
                strings,
                obj_ref_width,
            ) {
                Some((off, HprofType::Object)) => Some(off as usize),
                _ => None,
            }
        });
        if let Some(off) = name_off {
            if off + obj_ref_width <= blob.len() {
                let name_ref = read_ref(&blob[off..], obj_ref_width);
                if name_ref != 0 {
                    thread_to_name_addr.insert(addr, name_ref);
                }
            }
        }
    })?;
    if thread_to_name_addr.is_empty() {
        return Ok(names);
    }

    // ── Pass B: String addr → (array addr, coder) ────────────────────────────
    // Bounded by #threads (one name String each). The value/coder offsets are
    // resolved per String's own class id (memoized): Java 8 char[] Strings have
    // no `coder` field and are treated as UTF16 (coder 1).
    let wanted_strings: std::collections::HashSet<u64> =
        thread_to_name_addr.values().copied().collect();
    let mut string_to_arr: HashMap<u64, (u64, u8)> = HashMap::new();
    // class_id → (value_off, coder_off)
    let mut str_off_cache: HashMap<u64, Option<(usize, Option<usize>)>> = HashMap::new();
    scan_instance_blobs(path, id_size, &wanted_strings, |addr, class_id, blob| {
        let offs = *str_off_cache.entry(class_id).or_insert_with(|| {
            let value_off = match field_offset(
                class_id,
                "value",
                "java/lang/String",
                class_map,
                strings,
                obj_ref_width,
            ) {
                Some((off, HprofType::Object)) => off as usize,
                _ => return None,
            };
            let coder_off = match field_offset(
                class_id,
                "coder",
                "java/lang/String",
                class_map,
                strings,
                obj_ref_width,
            ) {
                Some((off, HprofType::Byte)) => Some(off as usize),
                _ => None,
            };
            Some((value_off, coder_off))
        });
        if let Some((value_off, coder_off)) = offs {
            if value_off + obj_ref_width <= blob.len() {
                let arr_ref = read_ref(&blob[value_off..], obj_ref_width);
                // Java 8 char[]: no coder field → UTF16 (coder 1).
                let coder = match coder_off {
                    Some(co) if co < blob.len() => blob[co],
                    _ => 1,
                };
                if arr_ref != 0 {
                    string_to_arr.insert(addr, (arr_ref, coder));
                }
            }
        }
    })?;
    if string_to_arr.is_empty() {
        return Ok(names);
    }

    // ── Pass C: array addr → element bytes ───────────────────────────────────
    // Bounded by #threads (each name array is tiny).
    let wanted_arrays: std::collections::HashSet<u64> =
        string_to_arr.values().map(|&(a, _)| a).collect();
    let mut arr_bytes: HashMap<u64, Vec<u8>> = HashMap::new();
    scan_prim_arrays(path, id_size, &wanted_arrays, |addr, bytes| {
        arr_bytes.insert(addr, bytes.to_vec());
    })?;

    // ── Decode: chain serial → thread → String → array → text ────────────────
    for (&serial, &thread_addr) in &p1.thread_serial_to_obj_id {
        let Some(&name_addr) = thread_to_name_addr.get(&thread_addr) else {
            continue;
        };
        let Some(&(arr_addr, coder)) = string_to_arr.get(&name_addr) else {
            continue;
        };
        let Some(bytes) = arr_bytes.get(&arr_addr) else {
            continue;
        };
        let text = decode_java_string(bytes, coder);
        if !text.is_empty() {
            names.insert(serial, text);
        }
    }

    Ok(names)
}

/// Maximum number of system-property entries captured. The props table is ONE
/// object, but its slot count is attacker/dump-controlled, so every worklist
/// derived from it is capped at this bound to keep RSS bounded regardless of
/// dump size.
const MAX_PROP_ENTRIES: usize = 4096;

/// Sorted `(key, value)` system-property pairs plus the derived JVM version.
type SystemProps = (Vec<(String, String)>, Option<String>);

/// Capture java.lang.System's static `props` object and decode it into a sorted
/// (key, value) list of system properties plus a derived JVM version.
///
/// Strategy (all passes bounded — see `MAX_PROP_ENTRIES`):
///   P0: scan CLASS_DUMP records for the class named `java/lang/System`; read
///       its static object field `props` → the props object address.
///   P1: props object → its `table` Object[] array address (Properties extends
///       Hashtable; `table` is declared by java/util/Hashtable). Java 9+
///       Properties that delegate to a ConcurrentHashMap have no such field →
///       graceful empty fallback.
///   P2: `table` Object[] → the non-null Hashtable$Entry slot addresses.
///   P3: entries → (key,value,next) refs; follow `next` chains (bounded by the
///       4096 cap) to collect all key/value String addresses.
///   P4: key/value Strings → (backing array addr, coder).
///   P5: backing PRIM_ARRAYs → raw bytes → decode.
///
/// Returns `(sorted (key,value) pairs, jvm_version)`. The JVM version is derived
/// even when the property table itself is empty (both keys come from the pairs).
/// On ANY layout mismatch the property list falls back to empty rather than
/// emitting garbage.
fn resolve_system_properties(path: &str, p1: &Pass1) -> io::Result<SystemProps> {
    let empty = (Vec::new(), None);
    let id_size = p1.id_size;
    let obj_ref_width = id_size as usize;
    let class_map = &p1.class_map;
    let strings = &p1.strings;

    // ── P0: locate java/lang/System's static `props` object address ──────────
    // Bounded: ONE class' static fields. Scan CLASS_DUMP records; for the class
    // whose name is "java/lang/System", read the OBJECT static field "props".
    let mut props_addr: u64 = 0;
    scan_class_dumps(path, id_size, |class_obj_id, statics| {
        if props_addr != 0 {
            return;
        }
        let cname = class_map
            .get(&class_obj_id)
            .and_then(|ci| strings.get(&ci.name_id))
            .map(|s| s.as_str())
            .unwrap_or("");
        if cname != "java/lang/System" {
            return;
        }
        for &(name_id, type_code, value) in statics {
            if HprofType::from_code(type_code) != Some(HprofType::Object) {
                continue;
            }
            let fname = strings.get(&name_id).map(|s| s.as_str()).unwrap_or("");
            if fname == "props" && value != 0 {
                props_addr = value;
            }
        }
    })?;
    if props_addr == 0 {
        return Ok(empty);
    }

    // ── P1: props object → its Hashtable `table` Object[] array address ───────
    // Bounded: ONE object. `table` is declared by java/util/Hashtable; matching
    // that owner avoids a subclass field of the same simple name shadowing it.
    let mut table_addr: u64 = 0;
    let wanted_props: std::collections::HashSet<u64> = std::iter::once(props_addr).collect();
    scan_instance_blobs(path, id_size, &wanted_props, |_addr, class_id, blob| {
        let off = match field_offset(
            class_id,
            "table",
            "java/util/Hashtable",
            class_map,
            strings,
            obj_ref_width,
        ) {
            Some((o, HprofType::Object)) => o as usize,
            _ => return,
        };
        if off + obj_ref_width <= blob.len() {
            table_addr = read_ref(&blob[off..], obj_ref_width);
        }
    })?;
    if table_addr == 0 {
        // No Hashtable `table` field (e.g. Java 9+ ConcurrentHashMap-backed
        // Properties). Fall back gracefully — no properties, no jvm_version.
        return Ok(empty);
    }

    // ── P2: `table` Object[] → non-null Hashtable$Entry slot addresses ────────
    // Bounded to MAX_PROP_ENTRIES.
    let wanted_table: std::collections::HashSet<u64> = std::iter::once(table_addr).collect();
    let mut entry_addrs: Vec<u64> = Vec::new();
    scan_obj_arrays(path, id_size, &wanted_table, |_addr, elem_refs| {
        for chunk in elem_refs.chunks_exact(obj_ref_width) {
            if entry_addrs.len() >= MAX_PROP_ENTRIES {
                break;
            }
            let r = read_ref(chunk, obj_ref_width);
            if r != 0 {
                entry_addrs.push(r);
            }
        }
    })?;
    if entry_addrs.is_empty() {
        return Ok(empty);
    }

    // ── P3: entries → (key,value,next) refs; follow `next` chains ─────────────
    // Bounded to MAX_PROP_ENTRIES entries total. Chains can add more entry
    // addresses to resolve, so iterate the worklist across repeated bounded
    // scans until it stabilizes (chains are short; capped by the entry budget).
    // key_val: entry addr → (key String addr, value String addr).
    let mut key_val: HashMap<u64, (u64, u64)> = HashMap::new();
    let mut pending: std::collections::HashSet<u64> = entry_addrs.iter().copied().collect();
    let mut entry_off_cache: HashMap<u64, Option<(usize, usize, usize)>> = HashMap::new();
    // Bound the number of chain-following passes; each pass resolves at least
    // one hop of every chain, so 64 caps the deepest Hashtable bucket chain we
    // will follow (real buckets are 1-3 deep). Combined with the entry budget
    // this is the fixed worst-case extra-pass ceiling; it terminates well before
    // it in practice.
    for _ in 0..64 {
        if pending.is_empty() || key_val.len() >= MAX_PROP_ENTRIES {
            break;
        }
        let mut next_pending: std::collections::HashSet<u64> = std::collections::HashSet::new();
        scan_instance_blobs(path, id_size, &pending, |addr, class_id, blob| {
            if key_val.contains_key(&addr) {
                return;
            }
            let offs = *entry_off_cache.entry(class_id).or_insert_with(|| {
                let key_off = match field_offset(
                    class_id,
                    "key",
                    "java/util/Hashtable$Entry",
                    class_map,
                    strings,
                    obj_ref_width,
                ) {
                    Some((o, HprofType::Object)) => o as usize,
                    _ => return None,
                };
                let value_off = match field_offset(
                    class_id,
                    "value",
                    "java/util/Hashtable$Entry",
                    class_map,
                    strings,
                    obj_ref_width,
                ) {
                    Some((o, HprofType::Object)) => o as usize,
                    _ => return None,
                };
                let next_off = match field_offset(
                    class_id,
                    "next",
                    "java/util/Hashtable$Entry",
                    class_map,
                    strings,
                    obj_ref_width,
                ) {
                    Some((o, HprofType::Object)) => o as usize,
                    _ => return None,
                };
                Some((key_off, value_off, next_off))
            });
            let Some((key_off, value_off, next_off)) = offs else {
                return;
            };
            if key_off + obj_ref_width > blob.len()
                || value_off + obj_ref_width > blob.len()
                || next_off + obj_ref_width > blob.len()
            {
                return;
            }
            let key_ref = read_ref(&blob[key_off..], obj_ref_width);
            let value_ref = read_ref(&blob[value_off..], obj_ref_width);
            let next_ref = read_ref(&blob[next_off..], obj_ref_width);
            key_val.insert(addr, (key_ref, value_ref));
            if next_ref != 0
                && !key_val.contains_key(&next_ref)
                && key_val.len() + next_pending.len() < MAX_PROP_ENTRIES
            {
                next_pending.insert(next_ref);
            }
        })?;
        pending = next_pending;
    }
    if key_val.is_empty() {
        return Ok(empty);
    }

    // ── P4: key/value Strings → (backing array addr, coder) ───────────────────
    // Bounded by 2 * #entries. Reuses the Stage-2 String decode field offsets.
    let mut wanted_strings: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for &(k, v) in key_val.values() {
        if k != 0 {
            wanted_strings.insert(k);
        }
        if v != 0 {
            wanted_strings.insert(v);
        }
    }
    let mut string_to_arr: HashMap<u64, (u64, u8)> = HashMap::new();
    let mut str_off_cache: HashMap<u64, Option<(usize, Option<usize>)>> = HashMap::new();
    scan_instance_blobs(path, id_size, &wanted_strings, |addr, class_id, blob| {
        let offs = *str_off_cache.entry(class_id).or_insert_with(|| {
            let value_off = match field_offset(
                class_id,
                "value",
                "java/lang/String",
                class_map,
                strings,
                obj_ref_width,
            ) {
                Some((off, HprofType::Object)) => off as usize,
                _ => return None,
            };
            let coder_off = match field_offset(
                class_id,
                "coder",
                "java/lang/String",
                class_map,
                strings,
                obj_ref_width,
            ) {
                Some((off, HprofType::Byte)) => Some(off as usize),
                _ => None,
            };
            Some((value_off, coder_off))
        });
        if let Some((value_off, coder_off)) = offs {
            if value_off + obj_ref_width <= blob.len() {
                let arr_ref = read_ref(&blob[value_off..], obj_ref_width);
                let coder = match coder_off {
                    Some(co) if co < blob.len() => blob[co],
                    _ => 1,
                };
                if arr_ref != 0 {
                    string_to_arr.insert(addr, (arr_ref, coder));
                }
            }
        }
    })?;

    // ── P5: backing PRIM_ARRAYs → raw bytes ───────────────────────────────────
    // Bounded by the number of distinct backing arrays (≤ 2 * #entries).
    let wanted_arrays: std::collections::HashSet<u64> =
        string_to_arr.values().map(|&(a, _)| a).collect();
    let mut arr_bytes: HashMap<u64, Vec<u8>> = HashMap::new();
    scan_prim_arrays(path, id_size, &wanted_arrays, |addr, bytes| {
        arr_bytes.insert(addr, bytes.to_vec());
    })?;

    // ── Decode: entry → key text, value text ─────────────────────────────────
    let decode = |str_addr: u64| -> Option<String> {
        if str_addr == 0 {
            return None;
        }
        let &(arr_addr, coder) = string_to_arr.get(&str_addr)?;
        let bytes = arr_bytes.get(&arr_addr)?;
        Some(decode_java_string(bytes, coder))
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    for &(k, v) in key_val.values() {
        let (Some(key), Some(value)) = (decode(k), decode(v)) else {
            continue;
        };
        if key.is_empty() {
            continue;
        }
        pairs.push((key, value));
    }
    // Deterministic: sort by key (then value), dedup exact duplicates.
    pairs.sort();
    pairs.dedup();

    // ── Derive JVM version: prefer java.vm.version, else java.version ─────────
    let find = |key: &str| -> Option<String> {
        pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    };
    let jvm_version = find("java.vm.version").or_else(|| find("java.version"));

    Ok((pairs, jvm_version))
}

/// whose object address is in `wanted`. Mirrors the heap-record scan skeleton
/// in `scan_heap_2a`/`fill_heap_2b` (streaming-only reader, per-segment
/// sub-record walk). Only the wanted objects' blobs are materialized; everything
/// else is skipped, so RSS stays bounded by `wanted`.
fn scan_instance_blobs<F: FnMut(u64, u64, &[u8])>(
    path: &str,
    id_size: u8,
    wanted: &std::collections::HashSet<u64>,
    mut f: F,
) -> io::Result<()> {
    let ids = id_size as u64;
    let mut r = HprofReader::open(path)?;
    let mut scratch: Vec<u8> = Vec::with_capacity(256);
    loop {
        let tag = match r.u1() {
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            other => other?,
        };
        let _ts = r.u4()?;
        let length = r.u4()? as u64;
        match tag {
            tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                let mut remaining = length;
                while remaining > 0 {
                    let sub_tag = r.u1()?;
                    remaining -= 1;
                    match sub_tag {
                        heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED | heap::ROOT_STICKY_CLASS => {
                            r.skip(ids)?;
                            remaining -= ids;
                        }
                        heap::ROOT_JNI_GLOBAL => {
                            r.skip(2 * ids)?;
                            remaining -= 2 * ids;
                        }
                        heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME | heap::ROOT_THREAD_OBJ => {
                            r.skip(ids + 8)?;
                            remaining -= ids + 8;
                        }
                        heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                            r.skip(ids + 4)?;
                            remaining -= ids + 4;
                        }
                        heap::HEAP_DUMP_INFO => {
                            r.skip(4 + ids)?;
                            remaining -= 4 + ids;
                        }
                        heap::CLASS_DUMP => {
                            let consumed = skip_class_dump(&mut r, id_size)?;
                            remaining -= consumed;
                        }
                        heap::INSTANCE_DUMP => {
                            let addr = r.id()?;
                            r.skip(4)?;
                            let class_id = r.id()?;
                            let data_len = r.u4()? as u64;
                            remaining -= ids + 4 + ids + 4 + data_len;
                            if wanted.contains(&addr) {
                                r.read_bytes_reuse(&mut scratch, data_len as usize)?;
                                f(addr, class_id, &scratch);
                            } else {
                                r.skip(data_len)?;
                            }
                        }
                        heap::OBJ_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            r.skip(ids)?;
                            let byte_len = count.saturating_mul(ids);
                            r.skip(byte_len)?;
                            remaining -= ids + 4 + 4 + ids + byte_len;
                        }
                        heap::PRIM_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            let elem_type = r.u1()?;
                            let esz = HprofType::from_code(elem_type)
                                .map(|t| t.byte_size() as u64)
                                .unwrap_or(1);
                            r.skip(count * esz)?;
                            remaining -= ids + 4 + 4 + 1 + count * esz;
                        }
                        other => {
                            return Err(io::Error::new(
                                ErrorKind::InvalidData,
                                format!("unknown heap sub-tag 0x{other:02x} in thread-name scan"),
                            ));
                        }
                    }
                }
            }
            tags::HEAP_DUMP_END => break,
            _ => r.skip(length)?,
        }
    }
    Ok(())
}

/// Full-file sequential scan invoking `f(addr, elem_bytes)` for each
/// PRIM_ARRAY_DUMP whose array address is in `wanted`. Only wanted arrays'
/// element bytes are materialized; everything else is skipped.
fn scan_prim_arrays<F: FnMut(u64, &[u8])>(
    path: &str,
    id_size: u8,
    wanted: &std::collections::HashSet<u64>,
    mut f: F,
) -> io::Result<()> {
    let ids = id_size as u64;
    let mut r = HprofReader::open(path)?;
    let mut scratch: Vec<u8> = Vec::with_capacity(256);
    loop {
        let tag = match r.u1() {
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            other => other?,
        };
        let _ts = r.u4()?;
        let length = r.u4()? as u64;
        match tag {
            tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                let mut remaining = length;
                while remaining > 0 {
                    let sub_tag = r.u1()?;
                    remaining -= 1;
                    match sub_tag {
                        heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED | heap::ROOT_STICKY_CLASS => {
                            r.skip(ids)?;
                            remaining -= ids;
                        }
                        heap::ROOT_JNI_GLOBAL => {
                            r.skip(2 * ids)?;
                            remaining -= 2 * ids;
                        }
                        heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME | heap::ROOT_THREAD_OBJ => {
                            r.skip(ids + 8)?;
                            remaining -= ids + 8;
                        }
                        heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                            r.skip(ids + 4)?;
                            remaining -= ids + 4;
                        }
                        heap::HEAP_DUMP_INFO => {
                            r.skip(4 + ids)?;
                            remaining -= 4 + ids;
                        }
                        heap::CLASS_DUMP => {
                            let consumed = skip_class_dump(&mut r, id_size)?;
                            remaining -= consumed;
                        }
                        heap::INSTANCE_DUMP => {
                            r.skip(ids + 4)?;
                            let _class_id = r.id()?;
                            let data_len = r.u4()? as u64;
                            r.skip(data_len)?;
                            remaining -= ids + 4 + ids + 4 + data_len;
                        }
                        heap::OBJ_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            r.skip(ids)?;
                            let byte_len = count.saturating_mul(ids);
                            r.skip(byte_len)?;
                            remaining -= ids + 4 + 4 + ids + byte_len;
                        }
                        heap::PRIM_ARRAY_DUMP => {
                            let addr = r.id()?;
                            r.skip(4)?;
                            let count = r.u4()? as u64;
                            let elem_type = r.u1()?;
                            let esz = HprofType::from_code(elem_type)
                                .map(|t| t.byte_size() as u64)
                                .unwrap_or(1);
                            let byte_len = count * esz;
                            remaining -= ids + 4 + 4 + 1 + byte_len;
                            if wanted.contains(&addr) {
                                r.read_bytes_reuse(&mut scratch, byte_len as usize)?;
                                f(addr, &scratch);
                            } else {
                                r.skip(byte_len)?;
                            }
                        }
                        other => {
                            return Err(io::Error::new(
                                ErrorKind::InvalidData,
                                format!("unknown heap sub-tag 0x{other:02x} in thread-name scan"),
                            ));
                        }
                    }
                }
            }
            tags::HEAP_DUMP_END => break,
            _ => r.skip(length)?,
        }
    }
    Ok(())
}

/// Full-file sequential scan invoking `f(addr, elem_ref_bytes)` for each
/// OBJ_ARRAY_DUMP whose array address is in `wanted`. `elem_ref_bytes` is the
/// raw block of `num_elements * id_size` reference bytes (element refs are
/// id_size wide inside an OBJ_ARRAY_DUMP, matching the array-element ref width
/// this scanner skips over). Only wanted arrays are materialized; everything
/// else is skipped, so RSS stays bounded by `wanted`.
fn scan_obj_arrays<F: FnMut(u64, &[u8])>(
    path: &str,
    id_size: u8,
    wanted: &std::collections::HashSet<u64>,
    mut f: F,
) -> io::Result<()> {
    let ids = id_size as u64;
    let mut r = HprofReader::open(path)?;
    let mut scratch: Vec<u8> = Vec::with_capacity(256);
    loop {
        let tag = match r.u1() {
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            other => other?,
        };
        let _ts = r.u4()?;
        let length = r.u4()? as u64;
        match tag {
            tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                let mut remaining = length;
                while remaining > 0 {
                    let sub_tag = r.u1()?;
                    remaining -= 1;
                    match sub_tag {
                        heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED | heap::ROOT_STICKY_CLASS => {
                            r.skip(ids)?;
                            remaining -= ids;
                        }
                        heap::ROOT_JNI_GLOBAL => {
                            r.skip(2 * ids)?;
                            remaining -= 2 * ids;
                        }
                        heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME | heap::ROOT_THREAD_OBJ => {
                            r.skip(ids + 8)?;
                            remaining -= ids + 8;
                        }
                        heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                            r.skip(ids + 4)?;
                            remaining -= ids + 4;
                        }
                        heap::HEAP_DUMP_INFO => {
                            r.skip(4 + ids)?;
                            remaining -= 4 + ids;
                        }
                        heap::CLASS_DUMP => {
                            let consumed = skip_class_dump(&mut r, id_size)?;
                            remaining -= consumed;
                        }
                        heap::INSTANCE_DUMP => {
                            r.skip(ids + 4)?;
                            let _class_id = r.id()?;
                            let data_len = r.u4()? as u64;
                            r.skip(data_len)?;
                            remaining -= ids + 4 + ids + 4 + data_len;
                        }
                        heap::OBJ_ARRAY_DUMP => {
                            let addr = r.id()?;
                            r.skip(4)?; // stack serial
                            let count = r.u4()? as u64;
                            r.skip(ids)?; // array class id
                            let byte_len = count.saturating_mul(ids);
                            remaining -= ids + 4 + 4 + ids + byte_len;
                            if wanted.contains(&addr) {
                                r.read_bytes_reuse(&mut scratch, byte_len as usize)?;
                                f(addr, &scratch);
                            } else {
                                r.skip(byte_len)?;
                            }
                        }
                        heap::PRIM_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            let elem_type = r.u1()?;
                            let esz = HprofType::from_code(elem_type)
                                .map(|t| t.byte_size() as u64)
                                .unwrap_or(1);
                            r.skip(count * esz)?;
                            remaining -= ids + 4 + 4 + 1 + count * esz;
                        }
                        other => {
                            return Err(io::Error::new(
                                ErrorKind::InvalidData,
                                format!("unknown heap sub-tag 0x{other:02x} in obj-array scan"),
                            ));
                        }
                    }
                }
            }
            tags::HEAP_DUMP_END => break,
            _ => r.skip(length)?,
        }
    }
    Ok(())
}

/// Full-file sequential scan invoking `f(class_obj_id, &statics)` for every
/// CLASS_DUMP sub-record, where `statics` is the captured list of static fields
/// as `(name_id, type_code, value)`. Object-typed values are id_size-wide refs;
/// primitive values are zero-extended into the u64. Only the (bounded) static
/// header of each class is materialized — instance-field descriptors are
/// skipped — so RSS stays O(#static-fields-of-one-class) inside the closure.
fn scan_class_dumps<F: FnMut(u64, &[(u64, u8, u64)])>(
    path: &str,
    id_size: u8,
    mut f: F,
) -> io::Result<()> {
    let ids = id_size as u64;
    let mut r = HprofReader::open(path)?;
    let mut statics: Vec<(u64, u8, u64)> = Vec::new();
    let mut vbuf: Vec<u8> = Vec::with_capacity(8);
    loop {
        let tag = match r.u1() {
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            other => other?,
        };
        let _ts = r.u4()?;
        let length = r.u4()? as u64;
        match tag {
            tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                let mut remaining = length;
                while remaining > 0 {
                    let sub_tag = r.u1()?;
                    remaining -= 1;
                    match sub_tag {
                        heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED | heap::ROOT_STICKY_CLASS => {
                            r.skip(ids)?;
                            remaining -= ids;
                        }
                        heap::ROOT_JNI_GLOBAL => {
                            r.skip(2 * ids)?;
                            remaining -= 2 * ids;
                        }
                        heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME | heap::ROOT_THREAD_OBJ => {
                            r.skip(ids + 8)?;
                            remaining -= ids + 8;
                        }
                        heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                            r.skip(ids + 4)?;
                            remaining -= ids + 4;
                        }
                        heap::HEAP_DUMP_INFO => {
                            r.skip(4 + ids)?;
                            remaining -= 4 + ids;
                        }
                        heap::CLASS_DUMP => {
                            let mut consumed = 0u64;
                            let class_obj_id = r.id()?;
                            r.skip(4)?; // stack serial
                            r.skip(ids * 6)?; // super, loader, signer, protdomain, r1, r2
                            r.skip(4)?; // instance size
                            consumed += ids + 4 + ids * 6 + 4;
                            // Constant pool: u2 count, entries (u2 idx, u1 type, value).
                            let cp_count = r.u2()?;
                            consumed += 2;
                            for _ in 0..cp_count {
                                r.skip(2)?;
                                let type_code = r.u1()?;
                                let vs = value_size(type_code, id_size);
                                r.skip(vs)?;
                                consumed += 2 + 1 + vs;
                            }
                            // Static fields: u2 count, entries (name_id, u1 type, value).
                            statics.clear();
                            let static_count = r.u2()?;
                            consumed += 2;
                            for _ in 0..static_count {
                                let name_id = r.id()?;
                                let type_code = r.u1()?;
                                let vs = value_size(type_code, id_size);
                                let value = if vs == 0 {
                                    0
                                } else {
                                    r.read_bytes_reuse(&mut vbuf, vs as usize)?;
                                    // Big-endian value; only OBJECT (id-wide)
                                    // values are consumed downstream, but decode
                                    // any width uniformly into the low bytes.
                                    let mut acc = 0u64;
                                    for &b in vbuf.iter() {
                                        acc = (acc << 8) | b as u64;
                                    }
                                    acc
                                };
                                consumed += ids + 1 + vs;
                                statics.push((name_id, type_code, value));
                            }
                            // Instance fields: u2 count, entries (name_id, u1 type).
                            let inst_count = r.u2()?;
                            consumed += 2;
                            for _ in 0..inst_count {
                                r.skip(ids)?;
                                r.skip(1)?;
                                consumed += ids + 1;
                            }
                            f(class_obj_id, &statics);
                            remaining -= consumed;
                        }
                        heap::INSTANCE_DUMP => {
                            r.skip(ids + 4)?;
                            let _class_id = r.id()?;
                            let data_len = r.u4()? as u64;
                            r.skip(data_len)?;
                            remaining -= ids + 4 + ids + 4 + data_len;
                        }
                        heap::OBJ_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            r.skip(ids)?;
                            let byte_len = count.saturating_mul(ids);
                            r.skip(byte_len)?;
                            remaining -= ids + 4 + 4 + ids + byte_len;
                        }
                        heap::PRIM_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            let elem_type = r.u1()?;
                            let esz = HprofType::from_code(elem_type)
                                .map(|t| t.byte_size() as u64)
                                .unwrap_or(1);
                            r.skip(count * esz)?;
                            remaining -= ids + 4 + 4 + 1 + count * esz;
                        }
                        other => {
                            return Err(io::Error::new(
                                ErrorKind::InvalidData,
                                format!("unknown heap sub-tag 0x{other:02x} in class-dump scan"),
                            ));
                        }
                    }
                }
            }
            tags::HEAP_DUMP_END => break,
            _ => r.skip(length)?,
        }
    }
    Ok(())
}

/// Skip a CLASS_DUMP sub-record, returning the byte count consumed AFTER the
/// 1-byte sub-tag (which the caller has already read). Mirrors the CLASS_DUMP
/// layout in pass1's `read_class_dump`: fixed header, constant pool, static
/// fields, instance-field descriptors.
fn skip_class_dump(r: &mut HprofReader, id_size: u8) -> io::Result<u64> {
    let ids = id_size as u64;
    let mut consumed = 0u64;
    // class_obj_id, stack_serial(4), super_id, loader_id, signer, protdomain,
    // reserved1, reserved2, instance_size(4)
    r.skip(ids)?; // class_obj_id
    r.skip(4)?; // stack serial
    r.skip(ids * 6)?; // super, loader, signer, protection domain, reserved1, reserved2
    r.skip(4)?; // instance_size
    consumed += ids + 4 + ids * 6 + 4;
    // Constant pool: u2 count, then entries of (u2 index, u1 type, value)
    let cp_count = r.u2()?;
    consumed += 2;
    for _ in 0..cp_count {
        r.skip(2)?; // constant pool index
        let type_code = r.u1()?;
        let vs = value_size(type_code, id_size);
        r.skip(vs)?;
        consumed += 2 + 1 + vs;
    }
    // Static fields: u2 count, then (name_id, u1 type, value)
    let static_count = r.u2()?;
    consumed += 2;
    for _ in 0..static_count {
        r.skip(ids)?; // name_id
        let type_code = r.u1()?;
        let vs = value_size(type_code, id_size);
        r.skip(vs)?;
        consumed += ids + 1 + vs;
    }
    // Instance fields: u2 count, then (name_id, u1 type)
    let inst_count = r.u2()?;
    consumed += 2;
    for _ in 0..inst_count {
        r.skip(ids)?; // name_id
        r.skip(1)?; // type
        consumed += ids + 1;
    }
    Ok(consumed)
}
/// line-number conventions (>0 = line; -1 unknown; -2 compiled; -3 native).
/// Missing strings fall back to placeholders so a frame is always printable.
fn render_frame(
    class: Option<&str>,
    method: Option<&str>,
    source: Option<&str>,
    class_serial: u32,
    line_number: i32,
) -> String {
    let class = class
        .map(|c| c.to_string())
        .unwrap_or_else(|| format!("<class#{class_serial}>"));
    let method = method.unwrap_or("<method>");
    let source = source.unwrap_or("Unknown Source");
    let loc = match line_number {
        n if n > 0 => format!("{source}:{n}"),
        -2 => format!("{source}(Compiled Method)"),
        -3 => "Native Method".to_string(),
        _ => source.to_string(),
    };
    format!("{class}.{method} ({loc})")
}

/// Convert an internal binary class name (`Lfoo/Bar;` or `foo/Bar`) into the
/// dotted display form used in stack frames (`foo.Bar`).
fn pretty_binary_name(name: &str) -> String {
    let trimmed = name.strip_prefix('L').unwrap_or(name);
    let trimmed = trimmed.strip_suffix(';').unwrap_or(trimmed);
    trimmed.replace('/', ".")
}

// ── Pass2 main logic ───────────────────────────────────────────────────────

pub struct Pass2;

impl Pass2 {
    pub fn build(
        path: &str,
        mut p1: Pass1,
        compress: crate::cvec::Codec,
    ) -> io::Result<(
        Graph,
        InboundBuilder,
        crate::cvec::CompressedU32,
        crate::cvec::CompressedU32,
    )> {
        let n = p1.id_map.len();
        let id_size = p1.id_size;
        let ptr_size = id_size as usize;

        // ── Phase 0: detect ref_size ─────────────────────────────────────
        // Reuse the object-array (addr, count) data already collected in pass1
        // instead of re-scanning the whole file. Array addresses are the id_map
        // entries whose kind == 1 (object array).
        let ref_size = if id_size == 8 {
            let mut array_addr_counts: Vec<(u64, u64)> = Vec::new();
            for i in 0..n {
                if p1.kind[i] == 1 {
                    array_addr_counts.push((p1.id_map.addr_at(i), p1.elem_count[i] as u64));
                }
            }
            detect_ref_size(id_size, &array_addr_counts)
        } else {
            id_size
        } as usize;

        // ── Phase 0b: compute shallow sizes with MAT formula ─────────────
        // Uses per-object kind (0=instance,1=obj_array,2=prim_array,3=class_obj)
        // and raw element counts collected in pass1 — authoritative, no heuristics.
        let mut size_cache: HashMap<u64, usize> = HashMap::new();

        // Set of class-object addresses (used later for edge/class-obj resolution).
        let class_addrs: std::collections::HashSet<u64> = p1.class_map.keys().cloned().collect();

        let mut shallow: Vec<u32> = Vec::with_capacity(n);
        for i in 0..n {
            let cid = p1.class_ids[i];
            let sz = match p1.kind[i] {
                3 => {
                    // Class object: shallow from static fields only, attributed to java.lang.Class.
                    let addr = p1.class_addr_table[cid as usize];
                    match p1.class_map.get(&addr) {
                        Some(ci) => class_obj_shallow(ci, ptr_size, ref_size),
                        None => align_up(ptr_size + ref_size, 8) as u32,
                    }
                }
                1 => {
                    // Object array: cid is the array class index (elem count from pass1).
                    obj_array_shallow(p1.elem_count[i] as u64, ptr_size, ref_size)
                }
                2 => {
                    // Primitive array: cid is the raw element type code.
                    let elem_size = HprofType::from_code(cid as u8)
                        .map(|t| t.byte_size())
                        .unwrap_or(1);
                    prim_array_shallow(p1.elem_count[i] as u64, elem_size, ptr_size, ref_size)
                }
                _ => {
                    // Instance: MAT calculateSizeRecursive over the super chain.
                    let addr = p1.class_addr_table[cid as usize];
                    if p1.class_map.contains_key(&addr) {
                        instance_shallow_size(
                            addr,
                            &p1.class_map,
                            ptr_size,
                            ref_size,
                            &mut size_cache,
                        )
                    } else {
                        align_up(ptr_size + ref_size, 8) as u32
                    }
                }
            };
            shallow.push(sz);
        }

        // ── Phase 0c: Build class names ──────────────────────────────────
        // MAT keys the class histogram by CLASS-OBJECT identity, not by name: a
        // class loaded by two different loaders yields two histogram rows even
        // though the names are identical. We therefore intern by a u64 key:
        //   - instances / object arrays: the class-object address (loader-distinct)
        //   - primitive arrays: PRIM_KEY_BASE | type_code (boot-loaded, single row)
        //   - class objects (java.lang.Class): the JLC_KEY sentinel (single row)
        const PRIM_KEY_BASE: u64 = 0xFFFF_0000_0000_0000;
        const JLC_KEY: u64 = 0xFFFF_FFFF_FFFF_FFFF;
        let mut class_key_to_idx: HashMap<u64, u32> = HashMap::new();
        let mut class_names: Vec<String> = Vec::new();
        let mut class_loader_id: Vec<u64> = Vec::new();

        let mut get_or_insert_class =
            |key: u64, name: &dyn Fn() -> String, loader: &dyn Fn() -> u64| -> u32 {
                if let Some(&idx) = class_key_to_idx.get(&key) {
                    return idx;
                }
                let idx = class_names.len() as u32;
                class_key_to_idx.insert(key, idx);
                class_names.push(name());
                class_loader_id.push(loader());
                idx
            };

        // Build class_idx array
        let mut class_idx: Vec<u32> = vec![0u32; n];

        // First pass: populate class_idx for all objects (kind-driven, no heuristics)
        for i in 0..n {
            let cid = p1.class_ids[i];

            match p1.kind[i] {
                3 => {
                    // Class object → single java/lang/Class row (MAT parity). Boot-loaded.
                    class_idx[i] =
                        get_or_insert_class(JLC_KEY, &|| "java/lang/Class".to_string(), &|| 0);
                }
                2 => {
                    // Primitive array: cid is the raw element type code. Boot-loaded.
                    let tc = cid as u8;
                    class_idx[i] = get_or_insert_class(
                        PRIM_KEY_BASE | tc as u64,
                        &|| prim_array_class_name(tc).to_string(),
                        &|| 0,
                    );
                }
                1 => {
                    // Object array: cid indexes the array-class address (loader-distinct).
                    let addr = p1.class_addr_table[cid as usize];
                    class_idx[i] = get_or_insert_class(
                        addr,
                        &|| {
                            p1.class_map
                                .get(&addr)
                                .and_then(|ci| p1.strings.get(&ci.name_id).cloned())
                                .unwrap_or_else(|| "[Ljava/lang/Object;".to_string())
                        },
                        &|| p1.class_map.get(&addr).map(|ci| ci.loader_id).unwrap_or(0),
                    );
                }
                _ => {
                    // Instance: cid indexes the class-object address (loader-distinct).
                    let addr = p1.class_addr_table[cid as usize];
                    class_idx[i] = get_or_insert_class(
                        addr,
                        &|| {
                            p1.class_map
                                .get(&addr)
                                .and_then(|ci| p1.strings.get(&ci.name_id).cloned())
                                .unwrap_or_else(|| format!("unknown@{addr:#x}"))
                        },
                        &|| p1.class_map.get(&addr).map(|ci| ci.loader_id).unwrap_or(0),
                    );
                }
            }
        }

        // Free pass1 per-object arrays that are dead after Phase 0b/0c: they
        // are only read to derive  and  above. Releasing
        // them here (~173 MB for a 11 M-object heap) shrinks peak RSS before
        // the edge-scan allocations (inb_flat / fwd_targets).
        //
        // NOTE: `class_ids`/`kind` are also read by the loader-label resolution
        // loop below, which must run AFTER the `get_or_insert_class` closure is
        // last used (~line 913, it holds a mutable borrow of `class_loader_id`).
        // We therefore keep `class_ids` alive until after that loop and free it
        // there; only the two vecs not needed by the loop are freed here.
        p1.shallow_sizes = Vec::new();
        p1.elem_count = Vec::new();

        // ── Phase 1: Sub-pass 2a — count degrees ────────────────────────
        let mut out_degree: Vec<u32> = vec![0u32; n];
        let mut in_degree: Vec<u32> = vec![0u32; n];
        crate::trace::probe("pass2: after out/in_degree alloc");

        // Precompute per-class instance-field plans once (offset + excluded flag).
        // Borrowed immutably in the hot scan loop — no per-instance allocation.
        let field_plans = build_field_plans(&p1.class_map, &p1.strings, id_size as usize);

        // ── Sub-pass 2a scan ─────────────────────────────────────────────
        {
            let mut r = HprofReader::open(path)?;
            // Scratch buffer reused across INSTANCE_DUMP and OBJ_ARRAY_DUMP reads (fix #6)
            let mut scratch: Vec<u8> = Vec::with_capacity(4096);

            loop {
                let tag = match r.u1() {
                    Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
                    other => other?,
                };
                let _ts = r.u4()?;
                let length = r.u4()? as u64;

                match tag {
                    tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                        Self::scan_heap_2a(
                            &mut r,
                            id_size,
                            ref_size,
                            ptr_size,
                            length,
                            &p1.id_map,
                            &p1.class_map,
                            &p1.strings,
                            &p1.kind,
                            &field_plans,
                            &mut size_cache,
                            &mut out_degree,
                            &mut in_degree,
                            &mut shallow,
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

        // Class objects already map to the java/lang/Class row (JLC_KEY) from Phase 0c.
        let jlc_idx = get_or_insert_class(JLC_KEY, &|| "java/lang/Class".to_string(), &|| 0);

        // ── Build class_obj_class_idx ─────────────────────────────────────
        // For each class object, record the histogram row of the class it
        // represents. Under identity keying, that row is keyed by the class
        // object's own address (the same key instances of that class use).
        let mut class_obj_class_idx: HashMap<u32, u32> = HashMap::new();
        for i in 0..n {
            let addr = p1.id_map.addr_at(i);
            if class_addrs.contains(&addr) {
                let ci = p1.class_map.get(&addr);
                let idx = get_or_insert_class(
                    addr,
                    &|| {
                        ci.and_then(|c| p1.strings.get(&c.name_id).cloned())
                            .unwrap_or_else(|| format!("unknown@{addr:#x}"))
                    },
                    &|| ci.map(|c| c.loader_id).unwrap_or(0),
                );
                class_obj_class_idx.insert(i as u32, idx);
            }
        }
        let _ = jlc_idx;

        // Resolve each distinct non-boot class-loader OBJECT address to the
        // class NAME of that loader object (e.g.
        // "jdk/internal/loader/ClassLoaders$AppClassLoader"), so the report
        // layer can label loaders instead of showing a raw address. Runs here,
        // AFTER the last `get_or_insert_class` use (that closure mutably borrows
        // `class_loader_id`), and BEFORE class_map/strings/id_map are freed or
        // moved (~lines 1013-1014, ~1198) and before `class_ids`/`kind` are
        // freed just below. Bounded by #distinct loaders (tens to low
        // hundreds), so it costs no per-object RSS. Boot loader (addr 0) is
        // labeled `<boot>` in the report layer.
        let mut loader_labels: std::collections::HashMap<u64, String> =
            std::collections::HashMap::new();
        for &loader_addr in &class_loader_id {
            if loader_addr == 0 {
                continue; // boot loader handled in report layer
            }
            if loader_labels.contains_key(&loader_addr) {
                continue;
            }
            // Resolve: loader_addr -> object index -> its class-obj addr -> name.
            if let Some(idx) = p1.id_map.index_of(loader_addr) {
                // Only plain instances (kind 0) are real loader objects.
                if p1.kind[idx] == 0 {
                    let cid = p1.class_ids[idx];
                    let class_addr = p1.class_addr_table[cid as usize];
                    if let Some(name) = p1
                        .class_map
                        .get(&class_addr)
                        .and_then(|ci| p1.strings.get(&ci.name_id))
                    {
                        loader_labels.insert(loader_addr, name.clone());
                    }
                }
            }
        }

        // Now free `class_ids` (see the free block above): the loader-label
        // loop was its last reader, so releasing it here keeps peak RSS low
        // before the edge-scan allocations.
        p1.class_ids = Vec::new();

        // TEMP DEBUG (env-gated, inert by default): dump every class-object
        // index -> address so a downstream reachability dump can join by index.
        if std::env::var_os("EXP_DUMP_CLASS_ADDRS").is_some() {
            use std::io::Write as _;
            if let Ok(f) = std::fs::File::create("/tmp/ours_class_idx_addr.txt") {
                let mut w = std::io::BufWriter::new(f);
                for &i in class_obj_class_idx.keys() {
                    let _ = writeln!(w, "{} 0x{:x}", i, p1.id_map.addr_at(i as usize));
                }
            }
            // Also probe the 9 MAT-only addresses against the FULL id_map (any
            // object kind) and against class_map (parsed CLASS_DUMP records).
            if let Ok(f) = std::fs::File::create("/tmp/ours_probe9.txt") {
                let mut w = std::io::BufWriter::new(f);
                for a in [
                    0xffe7f6f8u64,
                    0xffe7f768,
                    0xffe7f7d8,
                    0xffe7f848,
                    0xffe7f8b8,
                    0xffe7f928,
                    0xffe7f998,
                    0xffe7fa08,
                    0xffe7fa78,
                ] {
                    let in_idmap = p1.id_map.index_of(a).is_some();
                    let in_classmap = p1.class_map.contains_key(&a);
                    let _ = writeln!(
                        w,
                        "0x{:x} in_idmap={} in_classmap={}",
                        a, in_idmap, in_classmap
                    );
                }
            }
        }

        // Ensure no zero shallow sizes for instances/arrays (fall back to minimum).
        // Class objects (kind==3) are exempt: MAT reports 0 shallow for a class
        // whose static-field bytes sum to 0 (e.g. array classes like ), so we
        // must not bump those to the object minimum.
        let min_obj = align_up(ptr_size + ref_size, 8) as u32;
        for (i, s) in shallow.iter_mut().enumerate() {
            if *s == 0 && p1.kind[i] != 3 {
                *s = min_obj;
            }
        }

        // ── Phase 2: Build GC root indices ───────────────────────────────
        let mut gc_root_set: std::collections::HashSet<u32> = std::collections::HashSet::new();
        // Per-index representative root type (minimum sub-tag when an index has
        // several root records), carried into Graph for B1 grouping + why-alive.
        let mut root_type_of: std::collections::HashMap<u32, u8> = std::collections::HashMap::new();
        let note_type = |m: &mut std::collections::HashMap<u32, u8>, idx: u32, ty: u8| {
            m.entry(idx).and_modify(|e| *e = (*e).min(ty)).or_insert(ty);
        };
        for (&addr, &ty) in p1.gc_root_addrs.iter().zip(p1.gc_root_types.iter()) {
            if let Some(idx) = p1.id_map.index_of(addr) {
                gc_root_set.insert(idx as u32);
                note_type(&mut root_type_of, idx as u32, ty);
            }
        }
        // Add implicit roots: non-array boot-loader classes (loader_id==0) if no sticky roots
        if !p1.has_sticky_class_roots {
            for (&caddr, ci) in &p1.class_map {
                if ci.loader_id == 0 {
                    // Check it's not an array class (name doesn't start with '[')
                    let is_array = p1
                        .strings
                        .get(&ci.name_id)
                        .map(|n| n.starts_with('['))
                        .unwrap_or(false);
                    if !is_array {
                        if let Some(idx) = p1.id_map.index_of(caddr) {
                            gc_root_set.insert(idx as u32);
                            note_type(&mut root_type_of, idx as u32, heap::ROOT_SYSTEM_CLASS);
                        }
                    }
                }
            }
        }
        // ── addSystemClassRootsIfMissing: boot-loader non-array classes not yet roots ─
        let mut synthetic_root_count = 0usize;
        // MAT materializes a synthetic <system class loader> object at 0x0 of
        // class java/lang/ClassLoader (no HPROF record). Capture that class's
        // instance shallow size so the report layer can inject the object.
        let mut system_classloader_shallow: Option<u32> = None;
        for (&caddr, ci) in &p1.class_map {
            if ci.loader_id != 0 {
                continue;
            }
            let name = p1.strings.get(&ci.name_id);
            if name.map(|n| n == "java/lang/ClassLoader").unwrap_or(false) {
                system_classloader_shallow = Some(instance_shallow_size(
                    caddr,
                    &p1.class_map,
                    ptr_size,
                    ref_size,
                    &mut size_cache,
                ));
            }
            let is_array = name.map(|n| n.starts_with('[')).unwrap_or(false);
            let is_prim_array = name
                .map(|n| is_primitive_array_class_name(n))
                .unwrap_or(false);
            if !should_add_system_class_root(is_array, is_prim_array, p1.has_sticky_class_roots) {
                continue;
            }
            if let Some(idx) = p1.id_map.index_of(caddr) {
                if !gc_root_set.contains(&(idx as u32)) {
                    gc_root_set.insert(idx as u32);
                    note_type(&mut root_type_of, idx as u32, heap::ROOT_SYSTEM_CLASS);
                    synthetic_root_count += 1;
                }
            }
        }

        // Resolve STACK_TRACE/STACK_FRAME into pre-rendered thread stacks while
        // pass1's string/class tables are still alive (they are freed just
        // below). Only traces that carry frames are kept. Small — one entry per
        // thread trace, off the per-object RSS budget.
        let thread_stacks = build_thread_stacks(&p1);

        // Decode each thread's java.lang.Thread.name via a bounded 3-pass
        // worklist, while class_map/strings/id_map are still alive (freed just
        // below). All captured sets are bounded by #threads, so this stays off
        // the per-object RSS budget on multi-GB dumps.
        let thread_names = resolve_thread_names(path, &p1)?;

        // Capture java.lang.System's static `props` (a Properties/Hashtable of
        // String->String) via a bounded multi-pass worklist, while class_map/
        // strings/id_map are still alive. All captured sets are bounded (ONE
        // props object, capped at 4096 entries + their Strings/arrays), so this
        // stays off the per-object RSS budget on multi-GB dumps. Derives a JVM
        // version from the decoded properties. Falls back to empty/None (never
        // garbage) if the layout does not match the Hashtable form.
        let (system_properties, jvm_version) = resolve_system_properties(path, &p1)?;

        // class_map + strings are no longer needed; free before the large edge
        // arrays get allocated in Phase 3/4 to lower peak RSS. The STACK_FRAME/
        // STACK_TRACE maps were just consumed by build_thread_stacks and are
        // likewise dead — free them here too so they don't linger through the
        // peak-binding dominator/retained phases.
        p1.class_map = std::collections::HashMap::new();
        p1.strings = std::collections::HashMap::new();
        p1.stack_frames = std::collections::HashMap::new();
        p1.stack_traces = std::collections::HashMap::new();
        p1.stack_trace_thread = std::collections::HashMap::new();

        // ── Resolve thread→local synthetic edges ─────────────────────────
        let mut synthetic_edges: Vec<(u32, u32)> = Vec::new();
        // Per-thread count of local roots that resolve to a live object. Sized
        // by #threads only (bounded), so it stays off the per-object RSS budget.
        let mut thread_local_counts: std::collections::HashMap<u32, u64> =
            std::collections::HashMap::new();
        for &(thread_serial, local_addr) in &p1.thread_local_pairs {
            let thread_obj_addr = match p1.thread_serial_to_obj_id.get(&thread_serial) {
                Some(&a) => a,
                None => continue,
            };
            let thread_idx = match p1.id_map.index_of(thread_obj_addr) {
                Some(i) => i as u32,
                None => continue,
            };
            let local_idx = match p1.id_map.index_of(local_addr) {
                Some(i) => i as u32,
                None => continue,
            };
            if thread_idx != local_idx {
                synthetic_edges.push((thread_idx, local_idx));
                *thread_local_counts.entry(thread_serial).or_insert(0) += 1;
            }
        }
        // Dedup synthetic edges (same thread may reference same local multiple times)
        synthetic_edges.sort_unstable();
        synthetic_edges.dedup();

        // Add synthetic edge degrees to out_degree/in_degree
        for &(src, dst) in &synthetic_edges {
            out_degree[src as usize] += 1;
            in_degree[dst as usize] += 1;
        }

        let mut gc_root_indices: Vec<u32> = gc_root_set.into_iter().collect();
        gc_root_indices.sort_unstable();
        // Per-root type aligned 1:1 with the sorted indices. Every index in the
        // set came from a note_type call, so the lookup always hits; fall back
        // to ROOT_UNKNOWN defensively.
        let gc_root_types: Vec<u8> = gc_root_indices
            .iter()
            .map(|idx| root_type_of.get(idx).copied().unwrap_or(heap::ROOT_UNKNOWN))
            .collect();

        // Compress the two cold per-object arrays (shallow, class_idx) NOW,
        // before the forward-CSR fwd_targets (~6GB) is allocated. Both are
        // final here and idle until the retained phase, so freeing their dense
        // ~2GB Vecs each removes ~4GB from the binding fwd_targets-alloc peak.
        // main.rs holds the blobs across the peak and restores before consumers.
        let shallow_c = crate::cvec::CompressedU32::compress(&shallow, compress)?;
        if compress != crate::cvec::Codec::None {
            shallow = Vec::new();
        }
        let class_idx_c = crate::cvec::CompressedU32::compress(&class_idx, compress)?;
        if compress != crate::cvec::Codec::None {
            class_idx = Vec::new();
        }
        crate::trace::probe("pass2: after early-compress shallow/class_idx");

        // ── Phase 3: Build forward-CSR offsets (prefix sum only) ────────
        let mut fwd_offsets: Vec<u32> = Vec::with_capacity(n + 1);
        fwd_offsets.push(0u32);
        for i in 0..n {
            let next = fwd_offsets[i] + out_degree[i];
            fwd_offsets.push(next);
        }
        drop(out_degree); // dead after prefix sum
        crate::trace::probe("pass2: after fwd_offsets prefix-sum (out_degree freed)");

        // ── Phase 3b: Build forward CSR ──────────────────────────────────
        // The forward fill runs FIRST (inside build); the inbound CSR is
        // deferred into InboundBuilder so its ~5.5GB does not coexist with
        // the rpo phase's arrays. The forward fill never touches inb_flat.
        let total_edges = *fwd_offsets.last().unwrap() as usize;
        let mut fwd_targets: Vec<u32> = vec![u32::MAX; total_edges];
        crate::trace::probe("pass2: after fwd_targets alloc");
        // B3: no fwd_cursor clone. fwd_offsets is advanced in place as the
        // write cursor during the fill, then restored by right-shift below.
        {
            let mut r = HprofReader::open(path)?;
            let mut scratch: Vec<u8> = Vec::with_capacity(4096);
            let mut inb_flat_stub = crate::chunkvec::ChunkU32::zeroed(0);
            let mut in_degree_stub: Vec<u32> = Vec::new();
            loop {
                let tag = match r.u1() {
                    Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
                    other => other?,
                };
                let _ts = r.u4()?;
                let length = r.u4()? as u64;
                match tag {
                    tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                        Self::fill_heap_2b(
                            &mut r,
                            id_size,
                            ref_size,
                            length,
                            &p1.id_map,
                            &class_addrs,
                            &field_plans,
                            true,
                            false,
                            &mut fwd_targets,
                            &mut fwd_offsets,
                            &mut inb_flat_stub,
                            &mut in_degree_stub,
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
        // Synthetic thread->local FORWARD edges. Their degrees were added to
        // out_degree above, so each fits within its node's slice.
        for &(src, dst) in &synthetic_edges {
            let pos = fwd_offsets[src as usize] as usize;
            fwd_targets[pos] = dst;
            fwd_offsets[src as usize] += 1;
        }

        // B3 restore: each fwd_offsets[i] (i in 0..n) has advanced to node i's
        // END index; right-shift over (1..=n).rev() so fwd_offsets[node]..
        // fwd_offsets[node+1] again bounds node's slice. fwd_offsets[n]
        // (total_edges) was never a cursor and is preserved by starting at n.
        for i in (1..=n).rev() {
            fwd_offsets[i] = fwd_offsets[i - 1];
        }
        fwd_offsets[0] = 0;

        // Prefix-sum in_degree counts → START cursors for the deferred inbound
        // build. in_degree[i] becomes node i's inbound slice START; total_inb
        // is the flat inbound length. inb_flat is NOT allocated here.
        let mut total_inb: u64 = 0;
        for d in in_degree.iter_mut() {
            let cnt = *d as u64;
            *d = total_inb as u32;
            total_inb += cnt;
        }
        let in_cursors = in_degree; // renamed for clarity: prefix-summed START cursors

        // Precompute source_name before moving p1.id_map into InboundBuilder.
        let source_name = std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());

        let graph = Graph {
            n,
            format: p1.format,
            file_size: p1.file_size,
            source_name,
            file_path: path.to_string(),
            id_size,
            ref_size: ref_size as u8,
            header_timestamp_ms: p1.header_timestamp_ms,
            gc_root_indices,
            gc_root_types,
            shallow,
            class_idx,
            class_names,
            class_loader_id,
            loader_labels,
            thread_stacks,
            thread_names,
            thread_local_counts,
            system_properties,
            jvm_version,
            class_obj_class_idx,
            fwd_offsets,
            fwd_targets,
            synthetic_root_count,
            system_classloader_shallow,
            idom: Vec::new(),
            retained: Vec::new(),
            has_same_class_ancestor: crate::bitset::Bitset::default(),
        };

        // Package the deferred inbound-CSR construction. Moves id_map,
        // class_addrs, field_plans and synthetic_edges out of build (all
        // unused here after the forward fill).
        let inbound = InboundBuilder {
            path: path.to_string(),
            id_size,
            ref_size,
            n,
            id_map: Some(p1.id_map),
            id_map_c: None,
            id_map_codec: crate::cvec::Codec::None,
            class_addrs,
            field_plans,
            in_cursors,
            total_inb,
            synthetic_edges,
        };

        Ok((graph, inbound, shallow_c, class_idx_c))
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_heap_2a(
        r: &mut HprofReader,
        id_size: u8,
        ref_size: usize,
        ptr_size: usize,
        mut remaining: u64,
        id_map: &crate::id_map::IdMap,
        class_map: &HashMap<u64, ClassInfo>,
        _strings: &HashMap<u64, String>,
        kind: &[u8],
        field_plans: &HashMap<u64, FieldPlan>,
        size_cache: &mut HashMap<u64, usize>,
        out_degree: &mut Vec<u32>,
        in_degree: &mut Vec<u32>,
        shallow: &mut Vec<u32>,
        scratch: &mut Vec<u8>,
    ) -> io::Result<()> {
        let ids = id_size as u64;

        macro_rules! edge_if_valid {
            ($src:expr, $dst_addr:expr, $excl:expr) => {
                if $dst_addr != 0 {
                    if let Some(dst) = id_map.index_of($dst_addr) {
                        let src = $src as usize;
                        out_degree[src] += 1;
                        in_degree[dst] += 1;
                    }
                }
            };
        }

        macro_rules! checked_sub {
            ($remaining:expr, $sz:expr) => {
                $remaining = $remaining
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
                heap::ROOT_STICKY_CLASS => {
                    r.skip(ids)?;
                    checked_sub!(remaining, ids);
                }
                heap::ROOT_THREAD_OBJ => {
                    r.skip(ids + 8)?;
                    checked_sub!(remaining, ids + 8);
                }
                heap::CLASS_DUMP => {
                    let consumed =
                        Self::count_class_dump_edges(r, id_size, id_map, out_degree, in_degree)?;
                    checked_sub!(remaining, consumed);
                }
                heap::INSTANCE_DUMP => {
                    let addr = r.id()?;
                    r.skip(4)?;
                    let class_id = r.id()?;
                    let data_len = r.u4()? as u64;
                    if data_len > remaining {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "array too large",
                        ));
                    }
                    r.read_bytes_reuse(scratch, data_len as usize)?;
                    checked_sub!(remaining, ids + 4 + ids + 4 + data_len);

                    let src_idx = match id_map.index_of(addr) {
                        Some(i) => i,
                        None => continue,
                    };

                    // Recalculate MAT shallow size for instances (fix #3: reuse size_cache).
                    // kind[src_idx] == 3 marks class objects (pass1); equivalent
                    // to the old class_addrs.contains(addr) hash probe but reuses
                    // the already-computed src_idx (no per-instance hash lookup).
                    if kind[src_idx] != 3 {
                        let sz = instance_shallow_size(
                            class_id, class_map, ptr_size, ref_size, size_cache,
                        );
                        shallow[src_idx] = sz;
                    }

                    // Edge: instance → class object
                    edge_if_valid!(src_idx, class_id, false);

                    // Edges from Object-type fields (precomputed plan, immutable borrow)
                    if let Some(plan) = field_plans.get(&class_id) {
                        for &(off, _excluded) in plan {
                            let off = off as usize;
                            if off + id_size as usize <= scratch.len() {
                                let ref_val = read_ref(&scratch[off..], id_size as usize);
                                if ref_val != 0 {
                                    if let Some(dst) = id_map.index_of(ref_val) {
                                        out_degree[src_idx] += 1;
                                        in_degree[dst] += 1;
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
                    let elem_class_id = r.id()?;
                    let byte_len = count.saturating_mul(ids);
                    if byte_len > remaining {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "array too large",
                        ));
                    }
                    r.read_bytes_reuse(scratch, byte_len as usize)?;
                    checked_sub!(remaining, ids + 4 + 4 + ids + byte_len);

                    let src_idx = match id_map.index_of(addr) {
                        Some(i) => i,
                        None => continue,
                    };

                    // Fix shallow size for object arrays
                    shallow[src_idx] = obj_array_shallow(count, ptr_size, ref_size);

                    // class_idx[src_idx] already set by identity in Phase 0c.

                    // Edge: array → element class object
                    edge_if_valid!(src_idx, elem_class_id, false);

                    // Edges: array → non-null elements
                    for chunk in scratch.chunks(ids as usize) {
                        let ref_val = read_id(chunk, id_size);
                        if ref_val != 0 {
                            if let Some(dst) = id_map.index_of(ref_val) {
                                out_degree[src_idx] += 1;
                                in_degree[dst] += 1;
                            }
                        }
                    }
                }
                heap::PRIM_ARRAY_DUMP => {
                    let addr = r.id()?;
                    r.skip(4)?;
                    let count = r.u4()? as u64;
                    let elem_type = r.u1()?;
                    let esz = HprofType::from_code(elem_type)
                        .map(|t| t.byte_size() as u64)
                        .unwrap_or(1);
                    r.skip(count * esz)?;
                    checked_sub!(remaining, ids + 4 + 4 + 1 + count * esz);

                    if let Some(src_idx) = id_map.index_of(addr) {
                        // Fix shallow size (class_idx set by identity in Phase 0c)
                        shallow[src_idx] =
                            prim_array_shallow(count, esz as usize, ptr_size, ref_size);
                    }
                }
                other => {
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        format!("unknown heap sub-tag 0x{other:02x} in 2a"),
                    ));
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn fill_heap_2b(
        r: &mut HprofReader,
        id_size: u8,
        _ref_size: usize,
        mut remaining: u64,
        id_map: &crate::id_map::IdMap,
        _class_addrs: &std::collections::HashSet<u64>,
        field_plans: &HashMap<u64, FieldPlan>,
        do_fwd: bool,
        do_inb: bool,
        fwd_targets: &mut Vec<u32>,
        fwd_offsets: &mut Vec<u32>,
        inb_flat: &mut crate::chunkvec::ChunkU32,
        in_degree: &mut Vec<u32>,
        scratch: &mut Vec<u8>,
    ) -> io::Result<()> {
        let ids = id_size as u64;

        macro_rules! add_edge {
            ($src:expr, $dst_addr:expr, $excluded:expr) => {
                if $dst_addr != 0 {
                    if let Some(dst) = id_map.index_of($dst_addr) {
                        let src = $src as usize;
                        if do_fwd {
                            // fwd_offsets[src] is the in-place write cursor.
                            let pos = fwd_offsets[src] as usize;
                            fwd_targets[pos] = dst as u32;
                            fwd_offsets[src] += 1;
                        }
                        if do_inb {
                            // Inbound: store src with/without exclusion flag
                            let inb_val = if $excluded {
                                (src as u32) | 0x8000_0000u32
                            } else {
                                src as u32
                            };
                            inb_flat.set(in_degree[dst] as usize, inb_val);
                            in_degree[dst] += 1;
                        }
                    }
                }
            };
        }

        macro_rules! checked_sub {
            ($remaining:expr, $sz:expr) => {
                $remaining = $remaining
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
                heap::ROOT_STICKY_CLASS => {
                    r.skip(ids)?;
                    checked_sub!(remaining, ids);
                }
                heap::ROOT_THREAD_OBJ => {
                    r.skip(ids + 8)?;
                    checked_sub!(remaining, ids + 8);
                }
                heap::CLASS_DUMP => {
                    let consumed = Self::fill_class_dump_edges(
                        r,
                        id_size,
                        id_map,
                        do_fwd,
                        do_inb,
                        fwd_targets,
                        fwd_offsets,
                        inb_flat,
                        in_degree,
                    )?;
                    checked_sub!(remaining, consumed);
                }
                heap::INSTANCE_DUMP => {
                    let addr = r.id()?;
                    r.skip(4)?;
                    let class_id = r.id()?;
                    let data_len = r.u4()? as u64;
                    if data_len > remaining {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "array too large",
                        ));
                    }
                    r.read_bytes_reuse(scratch, data_len as usize)?;
                    checked_sub!(remaining, ids + 4 + ids + 4 + data_len);

                    let src_idx = match id_map.index_of(addr) {
                        Some(i) => i,
                        None => continue,
                    };

                    // Edge: instance → class object
                    add_edge!(src_idx, class_id, false);

                    // Edges from Object-type fields (precomputed plan, immutable borrow)
                    if let Some(plan) = field_plans.get(&class_id) {
                        for &(off, excluded) in plan {
                            let off = off as usize;
                            if off + id_size as usize <= scratch.len() {
                                let ref_val = read_ref(&scratch[off..], id_size as usize);
                                add_edge!(src_idx, ref_val, excluded);
                            }
                        }
                    }
                }
                heap::OBJ_ARRAY_DUMP => {
                    let addr = r.id()?;
                    r.skip(4)?;
                    let count = r.u4()? as u64;
                    let elem_class_id = r.id()?;
                    let byte_len = count.saturating_mul(ids);
                    if byte_len > remaining {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "array too large",
                        ));
                    }
                    r.read_bytes_reuse(scratch, byte_len as usize)?;
                    checked_sub!(remaining, ids + 4 + 4 + ids + byte_len);

                    let src_idx = match id_map.index_of(addr) {
                        Some(i) => i,
                        None => continue,
                    };

                    // Edge: array → element class
                    add_edge!(src_idx, elem_class_id, false);

                    // Edges to elements
                    for chunk in scratch.chunks(ids as usize) {
                        let ref_val = read_id(chunk, id_size);
                        add_edge!(src_idx, ref_val, false);
                    }
                }
                heap::PRIM_ARRAY_DUMP => {
                    let _addr = r.id()?;
                    r.skip(4)?;
                    let count = r.u4()? as u64;
                    let elem_type = r.u1()?;
                    let esz = HprofType::from_code(elem_type)
                        .map(|t| t.byte_size() as u64)
                        .unwrap_or(1);
                    r.skip(count * esz)?;
                    checked_sub!(remaining, ids + 4 + 4 + 1 + count * esz);
                    // No object edges from prim arrays
                }
                other => {
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        format!("unknown heap sub-tag 0x{other:02x} in 2b"),
                    ));
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn fill_class_dump_edges(
        r: &mut HprofReader,
        id_size: u8,
        id_map: &crate::id_map::IdMap,
        do_fwd: bool,
        do_inb: bool,
        fwd_targets: &mut Vec<u32>,
        fwd_offsets: &mut Vec<u32>,
        inb_flat: &mut crate::chunkvec::ChunkU32,
        in_degree: &mut Vec<u32>,
    ) -> io::Result<u64> {
        let ids = id_size as u64;
        let mut consumed = 0u64;

        let class_addr = r.id()?;
        consumed += ids;
        r.skip(4)?;
        consumed += 4;
        let super_id = r.id()?;
        consumed += ids;
        let loader_id = r.id()?;
        consumed += ids;
        r.skip(ids * 4 + 4)?;
        consumed += ids * 4 + 4;

        let src_idx_opt = id_map.index_of(class_addr);

        macro_rules! add_edge_inner {
            ($src:expr, $dst_addr:expr) => {
                if $dst_addr != 0 {
                    if let Some(dst) = id_map.index_of($dst_addr) {
                        let src = $src as usize;
                        if do_fwd {
                            // fwd_offsets[src] is the in-place write cursor.
                            let pos = fwd_offsets[src] as usize;
                            fwd_targets[pos] = dst as u32;
                            fwd_offsets[src] += 1;
                        }
                        if do_inb {
                            inb_flat.set(in_degree[dst] as usize, src as u32);
                            in_degree[dst] += 1;
                        }
                    }
                }
            };
        }

        if let Some(src) = src_idx_opt {
            add_edge_inner!(src, super_id);
            add_edge_inner!(src, loader_id);
        }

        // Constant pool
        let cp = r.u2()? as u64;
        consumed += 2;
        for _ in 0..cp {
            r.skip(2)?;
            consumed += 2;
            let tp = r.u1()?;
            consumed += 1;
            let vs = value_size(tp, id_size);
            r.skip(vs)?;
            consumed += vs;
        }

        // Static fields
        let sc = r.u2()? as u64;
        consumed += 2;
        for _ in 0..sc {
            r.skip(ids)?;
            consumed += ids; // name_id
            let tp = r.u1()?;
            consumed += 1;
            let vs = value_size(tp, id_size);
            if tp == 2 {
                // Object static field
                let ref_val = read_id_from_reader(r, id_size)?;
                consumed += vs;
                if let Some(src) = src_idx_opt {
                    add_edge_inner!(src, ref_val);
                }
            } else {
                r.skip(vs)?;
                consumed += vs;
            }
        }

        // Instance fields (just skip)
        let ic = r.u2()? as u64;
        consumed += 2;
        r.skip(ic * (ids + 1))?;
        consumed += ic * (ids + 1);

        Ok(consumed)
    }
}

// ── Also need 2a version of CLASS_DUMP to count static obj edges ───────────
// We need a version that also counts degrees for CLASS_DUMP static fields.

impl Pass2 {
    fn count_class_dump_edges(
        r: &mut HprofReader,
        id_size: u8,
        id_map: &crate::id_map::IdMap,
        out_degree: &mut Vec<u32>,
        in_degree: &mut Vec<u32>,
    ) -> io::Result<u64> {
        let ids = id_size as u64;
        let mut consumed = 0u64;

        let class_addr = r.id()?;
        consumed += ids;
        r.skip(4)?;
        consumed += 4;
        let super_id = r.id()?;
        consumed += ids;
        let loader_id = r.id()?;
        consumed += ids;
        r.skip(ids * 4 + 4)?;
        consumed += ids * 4 + 4;

        let src_opt = id_map.index_of(class_addr);

        macro_rules! count_edge {
            ($dst_addr:expr) => {
                if $dst_addr != 0 {
                    if let Some(dst) = id_map.index_of($dst_addr) {
                        if let Some(src) = src_opt {
                            out_degree[src] += 1;
                            in_degree[dst] += 1;
                        }
                    }
                }
            };
        }

        if src_opt.is_some() {
            count_edge!(super_id);
            count_edge!(loader_id);
        }

        let cp = r.u2()? as u64;
        consumed += 2;
        for _ in 0..cp {
            r.skip(2)?;
            consumed += 2;
            let tp = r.u1()?;
            consumed += 1;
            let vs = value_size(tp, id_size);
            r.skip(vs)?;
            consumed += vs;
        }

        let sc = r.u2()? as u64;
        consumed += 2;
        for _ in 0..sc {
            r.skip(ids)?;
            consumed += ids;
            let tp = r.u1()?;
            consumed += 1;
            let vs = value_size(tp, id_size);
            if tp == 2 {
                let ref_val = read_id_from_reader(r, id_size)?;
                consumed += vs;
                count_edge!(ref_val);
            } else {
                r.skip(vs)?;
                consumed += vs;
            }
        }

        let ic = r.u2()? as u64;
        consumed += 2;
        r.skip(ic * (ids + 1))?;
        consumed += ic * (ids + 1);

        Ok(consumed)
    }
}

// ── Utility ────────────────────────────────────────────────────────────────

/// Decode the backing element bytes of a `java.lang.String` into a Rust
/// `String`. `coder` follows the JDK 9+ `String.coder` convention:
///
/// - `0` = LATIN1: one byte per char, interpreted as ISO-8859-1.
/// - `1` = UTF16: two bytes per char, big-endian (HPROF byte order).
///
/// A JDK 8 `char[] value` has no `coder` field; callers pass `coder == 1`
/// because HPROF stores its chars as big-endian UTF-16 code units. Any other
/// `coder` value is treated as UTF16 (the only multi-byte case). Reusable by
/// later String-decoding stages.
pub fn decode_java_string(bytes: &[u8], coder: u8) -> String {
    if coder == 0 {
        // LATIN1 / ISO-8859-1: each byte is a Unicode code point 0..=255.
        bytes.iter().map(|&b| b as char).collect()
    } else {
        // UTF-16BE: pair bytes big-endian, lossily decode surrogates.
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    }
}

fn read_ref(data: &[u8], ref_size: usize) -> u64 {
    if ref_size == 4 {
        if data.len() >= 4 {
            u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as u64
        } else {
            0
        }
    } else if data.len() >= 8 {
        u64::from_be_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ])
    } else {
        0
    }
}

fn read_id(chunk: &[u8], id_size: u8) -> u64 {
    if id_size == 4 {
        if chunk.len() >= 4 {
            u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as u64
        } else {
            0
        }
    } else if chunk.len() >= 8 {
        u64::from_be_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ])
    } else {
        0
    }
}

fn read_id_from_reader(r: &mut HprofReader, _id_size: u8) -> io::Result<u64> {
    r.id()
}

fn value_size(type_code: u8, id_size: u8) -> u64 {
    match HprofType::from_code(type_code) {
        Some(HprofType::Object) => id_size as u64,
        Some(t) => t.byte_size() as u64,
        None => 0,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass1::Pass1;

    const DUMP: &str = "/home/i560383/test-heapdumps/dump_0_fj-kmeans.hprof";

    #[test]
    fn pretty_binary_name_strips_l_and_semicolon_and_dots() {
        assert_eq!(pretty_binary_name("Lfoo/bar/Baz;"), "foo.bar.Baz");
        assert_eq!(pretty_binary_name("foo/bar/Baz"), "foo.bar.Baz");
        assert_eq!(pretty_binary_name("Baz"), "Baz");
    }

    #[test]
    fn render_frame_applies_hprof_line_conventions() {
        assert_eq!(
            render_frame(Some("foo.Bar"), Some("run"), Some("Bar.java"), 7, 42),
            "foo.Bar.run (Bar.java:42)"
        );
        assert_eq!(
            render_frame(Some("foo.Bar"), Some("run"), Some("Bar.java"), 7, -1),
            "foo.Bar.run (Bar.java)"
        );
        assert_eq!(
            render_frame(Some("foo.Bar"), Some("run"), Some("Bar.java"), 7, -2),
            "foo.Bar.run (Bar.java(Compiled Method))"
        );
        assert_eq!(
            render_frame(Some("foo.Bar"), Some("run"), Some("Bar.java"), 7, -3),
            "foo.Bar.run (Native Method)"
        );
    }

    #[test]
    fn render_frame_falls_back_when_strings_missing() {
        assert_eq!(
            render_frame(None, None, None, 99, -1),
            "<class#99>.<method> (Unknown Source)"
        );
    }

    #[test]
    fn pass2_graph_has_edges() {
        if !std::path::Path::new(DUMP).exists() {
            return;
        }
        let p1 = Pass1::run(DUMP).unwrap();
        let (g, inbound, _sc, _ci) = Pass2::build(DUMP, p1, crate::cvec::Codec::None).unwrap();
        assert!(!g.fwd_targets.is_empty(), "no forward edges");
        assert_eq!(g.fwd_offsets.len(), g.n + 1);
        // Identity dfn (node -> pre-order) suffices. build() now returns
        // blocked offsets: one sampled offset per INB_BLOCK nodes + a trailing
        // sentinel, so len == ceil(n / INB_BLOCK) + 1.
        let dfn: Vec<u32> = (0..g.n as u32).collect();
        let (inb_block_off, _inb_data) = inbound.build(&dfn).unwrap();
        assert_eq!(inb_block_off.len(), g.n.div_ceil(INB_BLOCK) + 1);
        for &r in &g.gc_root_indices {
            assert!((r as usize) < g.n, "gc_root idx {} out of range {}", r, g.n);
        }
        assert_eq!(g.class_idx.len(), g.n);
        assert!(!g.class_names.is_empty());
        // Only class objects (e.g. array classes with no static fields) may have
        // shallow 0 — MAT reports 0 for those. All other objects must be > 0.
        for i in 0..g.n {
            if g.shallow[i] == 0 {
                assert!(
                    g.class_obj_class_idx.contains_key(&(i as u32)),
                    "non-class object {i} has shallow 0"
                );
            }
        }
    }

    #[test]
    fn pass2_edge_counts_sane() {
        if !std::path::Path::new(DUMP).exists() {
            return;
        }
        let p1 = Pass1::run(DUMP).unwrap();
        let (g, _inbound, _sc, _ci) = Pass2::build(DUMP, p1, crate::cvec::Codec::None).unwrap();
        let fwd_edge_count: usize = g
            .fwd_offsets
            .windows(2)
            .map(|w| (w[1] - w[0]) as usize)
            .sum();
        assert!(
            fwd_edge_count > g.n / 2,
            "suspiciously few edges: {} for {} nodes",
            fwd_edge_count,
            g.n
        );
    }

    // Opt 1 invariant: `kind[i] == 3` (class_obj) is EXACTLY equivalent to the
    // object's address being present in the `class_addrs` set that pass2 builds
    // from `class_map.keys()`. scan_heap_2a relies on this to replace the
    // per-instance `class_addrs.contains(addr)` hash probe with `kind[src] == 3`
    // (src already computed).
    #[test]
    fn kind3_equals_class_addrs_membership() {
        if !std::path::Path::new(DUMP).exists() {
            return;
        }
        let p1 = Pass1::run(DUMP).unwrap();
        let class_addrs: std::collections::HashSet<u64> = p1.class_map.keys().cloned().collect();
        assert!(!class_addrs.is_empty(), "expected some class objects");
        let mut class_count = 0usize;
        for i in 0..p1.id_map.len() {
            let addr = p1.id_map.addr_at(i);
            let is_class_by_kind = p1.kind[i] == 3;
            let is_class_by_set = class_addrs.contains(&addr);
            assert_eq!(
                is_class_by_kind, is_class_by_set,
                "kind==3 vs class_addrs.contains disagree at index {i} (addr {addr:#x})"
            );
            if is_class_by_kind {
                class_count += 1;
            }
        }
        assert_eq!(
            class_count,
            class_addrs.len(),
            "every class address must appear exactly once as a kind==3 object"
        );
    }

    #[test]
    fn primitive_array_class_name_recognizes_all_prims() {
        for n in ["[Z", "[C", "[F", "[D", "[S", "[I", "[J", "[B"] {
            assert!(
                is_primitive_array_class_name(n),
                "{n} should be a primitive-array class name"
            );
        }
    }

    #[test]
    fn primitive_array_class_name_rejects_non_prims() {
        for n in [
            "[[I",                 // multi-dim int array
            "[Ljava/lang/String;", // object array
            "java/lang/String",    // ordinary class
            "[",                   // lone bracket
            "[ZZ",                 // too long
            "",                    // empty
            "Z",                   // no bracket
            "[X",                  // bracket + non-prim char
        ] {
            assert!(
                !is_primitive_array_class_name(n),
                "{n:?} must NOT be a primitive-array class name"
            );
        }
    }

    #[test]
    fn system_class_rooting_matches_mat_addsystemclassroots() {
        // MAT's addSystemClassRootsIfMissing (HprofParserHandlerImpl.fillIn):
        // the class-rooting loop runs ONLY when no sticky/SYSTEM_CLASS roots
        // exist in the dump, and roots only non-array boot-loader classes.

        // The normal HPROF case: sticky roots present -> MAT roots nothing.
        // We match that for ordinary (non-array) boot-loader classes: this is
        // the fix for the big-dump +4,645-object frontier over-marking.
        assert!(
            !should_add_system_class_root(false, false, true),
            "non-array boot class must NOT be synthetically rooted when sticky roots exist"
        );
        // No sticky roots -> MAT (and we) root non-array boot-loader classes.
        assert!(
            should_add_system_class_root(false, false, false),
            "non-array boot class must be rooted when the dump has no sticky roots"
        );

        // Object arrays / multi-dim arrays are never synthetically rooted,
        // regardless of sticky presence (MAT guard: !clazz.isArrayType()).
        assert!(!should_add_system_class_root(true, false, false));
        assert!(!should_add_system_class_root(true, false, true));

        // Primitive-array metadata classes ([Z etc.) are ALWAYS rooted
        // (Group B mirror of MAT's dominator root-attachment), independent of
        // sticky-root presence.
        assert!(should_add_system_class_root(true, true, true));
        assert!(should_add_system_class_root(true, true, false));
    }

    #[test]
    fn decode_latin1_string() {
        // coder 0 = LATIN1: each byte is a code point 0..=255.
        assert_eq!(decode_java_string(b"main", 0), "main");
        assert_eq!(decode_java_string(&[0xe9], 0), "é"); // 0xE9 = U+00E9
        assert_eq!(decode_java_string(&[], 0), "");
    }

    #[test]
    fn decode_utf16be_string() {
        // coder 1 = UTF-16BE: pair bytes big-endian.
        // "main" as UTF-16BE.
        let utf16: Vec<u8> = "main"
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect();
        assert_eq!(decode_java_string(&utf16, 1), "main");
        // A non-Latin code point that needs UTF-16 (U+4E2D 中).
        let cjk: Vec<u8> = "中".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        assert_eq!(decode_java_string(&cjk, 1), "中");
    }

    #[test]
    fn decode_java8_char_array_is_utf16() {
        // Java 8 Strings have a char[] value and NO coder field; the resolver
        // passes coder 1 (UTF16) for them. A char[] holds UTF-16BE code units.
        let chars: Vec<u8> = "hi".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        assert_eq!(decode_java_string(&chars, 1), "hi");
    }

    #[test]
    fn field_offset_places_superclass_fields_after_subclass_fields() {
        // HPROF stores instance field VALUES subclass-first: the object's own
        // class fields precede the inherited superclass fields in the blob. Build
        // a synthetic two-class chain and confirm the inherited field's offset
        // lands *after* the subclass's own fields, and that the owner_class
        // filter skips a same-named field declared by the subclass.
        let mut strings: HashMap<u64, String> = HashMap::new();
        strings.insert(1, "java/lang/Thread".to_string());
        strings.insert(2, "Sub".to_string());
        strings.insert(10, "eetop".to_string()); // Thread field (Long)
        strings.insert(11, "name".to_string()); // Thread field (Object)
        strings.insert(20, "extra".to_string()); // Sub field (Int)
        strings.insert(21, "name".to_string()); // Sub's OWN shadowing "name"

        let obj_ref_width = 8usize;
        let thread = ClassInfo {
            name_id: 1,
            super_id: 0,
            fields: vec![(10, HprofType::Long), (11, HprofType::Object)],
            ..Default::default()
        };
        let sub = ClassInfo {
            name_id: 2,
            super_id: 100, // points at Thread
            fields: vec![(20, HprofType::Int), (21, HprofType::Object)],
            ..Default::default()
        };
        let mut class_map: HashMap<u64, ClassInfo> = HashMap::new();
        class_map.insert(100, thread);
        class_map.insert(200, sub);

        // Sub's own fields (int=4 + object=8) come first = 12 bytes, then Thread:
        // eetop(Long=8), then name(Object) at 12 + 8 = 20.
        let (off, t) = field_offset(
            200,
            "name",
            "java/lang/Thread",
            &class_map,
            &strings,
            obj_ref_width,
        )
        .expect("inherited Thread.name must resolve");
        assert_eq!(off, 20);
        assert_eq!(t, HprofType::Object);

        // For a pure java/lang/Thread instance, name is right after eetop = 8.
        let (off2, _) = field_offset(
            100,
            "name",
            "java/lang/Thread",
            &class_map,
            &strings,
            obj_ref_width,
        )
        .expect("Thread.name must resolve");
        assert_eq!(off2, 8);
    }
}
