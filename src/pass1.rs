//! Pass 1: the first scan over the heap dump. It reads STRING/LOAD_CLASS/
//! STACK_FRAME/STACK_TRACE records and every heap-dump sub-record, building the
//! `id_map` and one entry per object across a set of parallel per-object arrays
//! (`class_ids`, `kind`, `elem_count`, and optionally `alloc_stack_serial`)
//! that stay 1:1 aligned by index. It also records class/thread metadata and
//! GC roots. This pass is on the peak-RSS-critical path and its output feeds
//! byte-exact parity tests, so it is written to keep large scratch arrays from
//! coexisting. Shallow sizes are NOT built here — pass2 Phase 0b recomputes
//! them with the authoritative MAT formula.

use std::{
    collections::HashMap,
    io::{self, ErrorKind},
};

use crate::{
    id_map::IdMap,
    pass2::sub_remaining,
    reader::HprofReader,
    types::{HprofType, heap, tags},
};

/// A thread-local GC-root edge: `(threadSerial, frameNumber, localAddr)`.
/// `frameNumber` is the 0-based stack-frame index (topmost = 0) for
/// ROOT_JAVA_FRAME; `u32::MAX` means "no associated frame" (JNI local / native
/// stack / thread block).
pub type ThreadLocalRoot = (u32, u32, u64);

/// One HPROF STACK_FRAME (0x04) record. String ids resolve against `strings`;
/// `class_serial` resolves against `class_serial_to_addr` → `class_map`.
/// `line_number` uses the HPROF conventions (>0 = line; -1 unknown; -2 compiled
/// method; -3 native method) stored as the raw i32.
#[derive(Debug, Clone, Copy, Default)]
pub struct StackFrame {
    pub method_name_id: u64,
    pub source_file_id: u64,
    pub class_serial: u32,
    pub line_number: i32,
}

/// Per-class metadata gathered from LOAD_CLASS + CLASS_DUMP records.
#[derive(Debug, Default)]
pub struct ClassInfo {
    pub name_id: u64,
    pub super_id: u64,
    pub loader_id: u64,
    pub instance_size: u32,
    /// Instance fields in HPROF order (subclass fields first): (name_id, type)
    pub fields: Vec<(u64, HprofType)>,
    /// Number of static object fields (for class-object shallow size)
    pub static_obj_count: u32,
    /// Number of static primitive bytes (for class-object shallow size)
    pub static_prim_bytes: u32,
}

/// Result of pass 1: interned strings, class/thread metadata, the `id_map`, and
/// the parallel per-object arrays (all 1:1 by index) that pass 2 consumes.
pub struct Pass1 {
    /// String id → decoded UTF-8 (lossy) from STRING_IN_UTF8 records.
    pub strings: HashMap<u64, String>,
    /// Class-object address → per-class metadata.
    pub class_map: HashMap<u64, ClassInfo>,
    /// LOAD_CLASS serial → class-object address.
    pub class_serial_to_addr: HashMap<u32, u64>,
    /// Two-level map from object address to dense object index (unique, sorted).
    pub id_map: IdMap,
    /// Per-object class reference, u32-interned to halve this array (was
    /// Vec<u64> class addresses @514M = 4.1GB). For kind 0/1/3 it is an index
    /// into class_addr_table (the distinct class-object addresses); for kind 2
    /// (primitive array) it is the raw element type code (0-11).
    pub class_ids: Vec<u32>,
    /// Distinct class-object addresses; class_ids[i] indexes this for kind 0/1/3.
    pub class_addr_table: Vec<u64>,
    /// Per-object HPROF allocation stack-trace serial (u4), 1:1 with the other
    /// per-object parallel arrays. Only populated when `--alloc-sites` capture
    /// is on; otherwise left empty so the default path costs zero extra RSS.
    /// CLASS_DUMP object slots carry 0 (no per-object alloc serial). HotSpot
    /// writes 0 when allocation tracking is off.
    pub alloc_stack_serial: Vec<u32>,
    /// Per-object kind: 0=instance, 1=obj_array, 2=prim_array, 3=class_obj
    pub kind: Vec<u8>,
    /// Per-object raw element count (arrays only; 0 otherwise)
    pub elem_count: Vec<u32>,
    /// Addresses of direct GC roots (excludes thread-local synthetic edges).
    pub gc_root_addrs: Vec<u64>,
    /// Per-root GC-root sub-tag, 1:1 with `gc_root_addrs`.
    #[allow(dead_code)]
    pub gc_root_types: Vec<u8>,
    /// threadSerial → thread object address (from ROOT_THREAD_OBJ records)
    pub thread_serial_to_obj_id: HashMap<u32, u64>,
    /// Thread-local GC-root edges from JAVA_FRAME/JNI_LOCAL/NATIVE_STACK/THREAD_BLOCK.
    pub thread_local_pairs: Vec<ThreadLocalRoot>,
    /// STACK_FRAME (0x04) records, keyed by frame_id. Small (thousands), off
    /// the per-object RSS budget.
    pub stack_frames: HashMap<u64, StackFrame>,
    /// STACK_TRACE (0x05) records: stack_serial → ordered frame_ids (top frame
    /// first). Small (hundreds), off the per-object RSS budget.
    pub stack_traces: HashMap<u32, Vec<u64>>,
    /// STACK_TRACE (0x05): stack_serial → thread_serial (0 = no thread).
    pub stack_trace_thread: HashMap<u32, u32>,
    /// Object-id byte width from the HPROF header (typically 8).
    pub id_size: u8,
    /// HPROF format string from the header (e.g. "JAVA PROFILE 1.0.2").
    pub format: String,
    /// Total dump file size in bytes.
    pub file_size: u64,
    /// HPROF header base timestamp (millis since Unix epoch), 0 if absent.
    pub header_timestamp_ms: u64,
    /// True if any ROOT_STICKY_CLASS root was seen.
    pub has_sticky_class_roots: bool,
    // Validation counters
    /// INSTANCE_DUMP records seen.
    pub instance_count: u64,
    /// OBJ_ARRAY_DUMP records seen.
    pub obj_array_count: u64,
    /// PRIM_ARRAY_DUMP records seen.
    pub prim_array_count: u64,
    /// CLASS_DUMP records seen.
    pub class_dump_count: u64,
    /// STRING_IN_UTF8 (0x01) records seen.
    pub utf8_records: u64,
    /// LOAD_CLASS (0x02) records seen.
    pub load_class_records: u64,
    /// UNLOAD_CLASS (0x03) records seen (previously skipped/uncounted).
    pub unload_class_records: u64,
    /// STACK_FRAME (0x04) records seen.
    pub stack_frame_records: u64,
    /// STACK_TRACE (0x05) records seen.
    pub stack_trace_records: u64,
    /// HEAP_DUMP + HEAP_DUMP_SEGMENT (0x0c/0x1c) top-level records seen.
    pub heap_dump_segments: u64,
    /// Per-GC-root-tag counts, keyed by the HPROF root sub-tag byte. Covers
    /// every root sub-record encountered, including the thread-local ones
    /// (ROOT_JNI_LOCAL/JAVA_FRAME/NATIVE_STACK/THREAD_BLOCK) that become
    /// synthetic edges rather than direct GC roots.
    pub gc_root_tag_counts: std::collections::HashMap<u8, u64>,
}

impl Pass1 {
    /// Runs pass 1 over the dump at `path`. Always records each object's
    /// allocation stack-trace serial (for the always-on alloc-sites report);
    /// serials are 0 unless the JVM ran with allocation tracking enabled.
    pub fn run(path: &str) -> io::Result<Self> {
        let file_size = std::fs::metadata(path)?.len();
        let mut r = HprofReader::open(path)?;
        let id_size = r.id_size;
        let format = r.format.clone();
        // Header base timestamp (u8 millis-since-epoch after id_size), read by
        // HprofReader::open. This is the dump creation time (not the per-record
        // microsecond delta at `_timestamp` below).
        let header_timestamp_ms = r.timestamp_ms;

        let mut strings: HashMap<u64, String> = HashMap::default();
        let mut class_map: HashMap<u64, ClassInfo> = HashMap::new();
        let mut class_serial_to_addr: HashMap<u32, u64> = HashMap::default();
        let mut tmp_addrs: Vec<u64> = Vec::new();
        let mut tmp_class_ids: Vec<u32> = Vec::new();
        // Intern class addresses to u32 during scan (heaps have < 4G classes;
        // this dump ~133K). kind 2 stores the raw type code instead.
        // kind (2 bits, values 0-3) is packed into bits 30-31 of tmp_class_ids
        // to eliminate a separate 512 MB Vec<u8> from the sort-peak window.
        // CLASS_MASK strips those bits when accessing the class index or type code.
        let mut class_addr_table: Vec<u64> = Vec::new();
        let mut class_addr_to_idx: HashMap<u64, u32> = HashMap::default();
        let mut tmp_elem_count: Vec<u32> = Vec::new();
        // Per-object alloc stack-trace serial. Only grown when capturing; stays
        // empty (zero RSS) on the default path.
        let mut tmp_alloc_serial: Vec<u32> = Vec::new();
        let mut gc_root_addrs: Vec<u64> = Vec::new();
        let mut gc_root_types: Vec<u8> = Vec::new();
        let mut thread_serial_to_obj_id: HashMap<u32, u64> = HashMap::default();
        let mut thread_local_pairs: Vec<ThreadLocalRoot> = Vec::new();
        let mut stack_frames: HashMap<u64, StackFrame> = HashMap::default();
        let mut stack_traces: HashMap<u32, Vec<u64>> = HashMap::new();
        let mut stack_trace_thread: HashMap<u32, u32> = HashMap::default();
        let mut has_sticky_class_roots = false;
        let mut instance_count = 0u64;
        let mut obj_array_count = 0u64;
        let mut prim_array_count = 0u64;
        let mut class_dump_count = 0u64;
        let mut utf8_records = 0u64;
        let mut load_class_records = 0u64;
        let mut unload_class_records = 0u64;
        let mut stack_frame_records = 0u64;
        let mut stack_trace_records = 0u64;
        let mut heap_dump_segments = 0u64;
        let mut gc_root_tag_counts: std::collections::HashMap<u8, u64> =
            std::collections::HashMap::new();

        loop {
            let tag = match r.u1() {
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
                other => other?,
            };
            let _timestamp = r.u4()?;
            let length = r.u4()? as u64;

            match tag {
                tags::STRING_IN_UTF8 => {
                    utf8_records += 1;
                    let str_id = r.id()?;
                    // Body = id(8) + UTF-8 bytes. A truncated/corrupt record with
                    // `length < id_size` would underflow the subtraction to a
                    // near-u64::MAX byte count, triggering a ~16 EiB allocation in
                    // `read_bytes` (abort/OOM). Reject it as malformed instead.
                    let payload_len = length.checked_sub(id_size as u64).ok_or_else(|| {
                        io::Error::new(
                            ErrorKind::InvalidData,
                            "STRING_IN_UTF8 record length shorter than id size",
                        )
                    })?;
                    let bytes = r.read_bytes(payload_len as usize)?;
                    strings.insert(str_id, String::from_utf8_lossy(&bytes).into_owned());
                }
                tags::LOAD_CLASS => {
                    load_class_records += 1;
                    // serial(4) + class_addr(id) + stack_serial(4) + name_id(id)
                    let serial = r.u4()?;
                    let class_addr = r.id()?;
                    let _stack_serial = r.u4()?;
                    let name_id = r.id()?;
                    class_serial_to_addr.insert(serial, class_addr);
                    class_map.entry(class_addr).or_default().name_id = name_id;
                }
                tags::STACK_FRAME => {
                    stack_frame_records += 1;
                    // frame_id(id) + method_name_id(id) + method_sig_id(id)
                    // + source_file_id(id) + class_serial(u4) + line_number(u4)
                    let frame_id = r.id()?;
                    let method_name_id = r.id()?;
                    let _method_sig_id = r.id()?;
                    let source_file_id = r.id()?;
                    let class_serial = r.u4()?;
                    let line_number = r.u4()? as i32;
                    stack_frames.insert(
                        frame_id,
                        StackFrame {
                            method_name_id,
                            source_file_id,
                            class_serial,
                            line_number,
                        },
                    );
                }
                tags::STACK_TRACE => {
                    stack_trace_records += 1;
                    // stack_serial(u4) + thread_serial(u4) + num_frames(u4)
                    // + frame_id[num_frames](id)
                    let stack_serial = r.u4()?;
                    let thread_serial = r.u4()?;
                    let num_frames = r.u4()?;
                    let mut frames = Vec::with_capacity(num_frames as usize);
                    for _ in 0..num_frames {
                        frames.push(r.id()?);
                    }
                    stack_traces.insert(stack_serial, frames);
                    stack_trace_thread.insert(stack_serial, thread_serial);
                }
                tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                    heap_dump_segments += 1;
                    scan_heap_segment(
                        &mut r,
                        id_size,
                        length,
                        &mut class_map,
                        &mut tmp_addrs,
                        &mut tmp_class_ids,
                        &mut class_addr_table,
                        &mut class_addr_to_idx,
                        &mut tmp_elem_count,
                        &mut tmp_alloc_serial,
                        &mut gc_root_addrs,
                        &mut gc_root_types,
                        &mut thread_serial_to_obj_id,
                        &mut thread_local_pairs,
                        &mut has_sticky_class_roots,
                        &mut instance_count,
                        &mut obj_array_count,
                        &mut prim_array_count,
                        &mut class_dump_count,
                        &mut gc_root_tag_counts,
                    )?;
                }
                tags::HEAP_DUMP_END => break,
                tags::UNLOAD_CLASS => {
                    unload_class_records += 1;
                    r.skip(length)?;
                }
                _ => {
                    r.skip(length)?;
                }
            }
        }

        crate::trace::probe("pass1: after scan loop (all tmp_* grown)");
        // Free the reader buffer and no-longer-needed intern map before the
        // sort to trim the working set as much as possible before allocating order.
        drop(r);
        drop(class_addr_to_idx);
        crate::trace::trim(); // return allocator free-list to OS before sort peak

        // Sort by address and deduplicate (same address may appear under
        // multiple roots). `order` is a u32 permutation (heaps hold < 4 G
        // objects, so u32 suffices). To keep `order` (2 GB @514M) off the
        // binding pass1 peak, we apply it IN PLACE to every parallel array so
        // the arrays themselves become address-sorted, then DROP `order`
        // before building the id_map offsets and deduping — so `order`,
        // `tmp_addrs`, and `id_map.offsets` never coexist (that 3-way overlap
        // was the ~14.9 GB peak).
        // Note: tmp_kind is eliminated — kind bits are packed into tmp_class_ids
        // upper 2 bits, saving ~512 MB during the sort window.
        let n = tmp_addrs.len();
        let mut order: Vec<u32> = (0..n as u32).collect();
        crate::trace::probe("pass1: before order.sort_unstable_by_key");
        order.sort_unstable_by_key(|&i| tmp_addrs[i as usize]);
        crate::trace::probe("pass1: after order.sort_unstable_by_key");

        // Permute all four parallel arrays into address-sorted order in place
        // (no output allocation). `order` is consumed as scratch (top-bit
        // marker) and dropped immediately after — freeing 2 GB before the
        // dedup/offsets pass below.
        apply_permutation_in_place(&mut order, |x, y| {
            tmp_addrs.swap(x, y);
            tmp_class_ids.swap(x, y);
            tmp_elem_count.swap(x, y);
            // Only permute the alloc-serial array when it was actually captured
            // (1:1 with the others). Empty on the default path — leave untouched.
            if !tmp_alloc_serial.is_empty() {
                tmp_alloc_serial.swap(x, y);
            }
        });
        drop(order);
        crate::trace::probe("pass1: after drop(order) (arrays sorted in place)");

        // Sequential dedup+build over the now address-sorted arrays: feed each
        // distinct (strictly-ascending) address into the two-level id_map and
        // compact the payload arrays in place to the unique prefix [0, write).
        // No 4.1GB staging Vec, no separate gather passes.
        let mut id_map = IdMap::new();
        id_map.reserve_offsets(n);
        let mut prev_addr = u64::MAX;
        let mut write = 0usize;
        for rank in 0..n {
            let a = tmp_addrs[rank];
            if a != prev_addr {
                id_map.push_sorted_addr(a);
                if write != rank {
                    // Compact every parallel array by the SAME (write, rank)
                    // shift so all payloads stay 1:1 aligned by index.
                    tmp_class_ids[write] = tmp_class_ids[rank];
                    tmp_elem_count[write] = tmp_elem_count[rank];
                    if !tmp_alloc_serial.is_empty() {
                        tmp_alloc_serial[write] = tmp_alloc_serial[rank];
                    }
                }
                write += 1;
                prev_addr = a;
            }
        }
        crate::trace::probe("pass1: before id_map.finalize (tmp_addrs+payloads live, order freed)");
        id_map.finalize_sorted();
        crate::trace::probe("pass1: after id_map.finalize (offsets built)");
        let m = id_map.len();
        debug_assert_eq!(m, write, "id_map len must equal unique-address count");
        drop(tmp_addrs);
        crate::trace::probe("pass1: after drop(tmp_addrs)");

        // The payload arrays are already compacted (unique, address-sorted) in
        // their prefix [0, m); truncate and reuse them directly as outputs.
        tmp_class_ids.truncate(m);
        tmp_elem_count.truncate(m);
        // Only truncate the alloc-serial array when captured (else it is empty).
        if !tmp_alloc_serial.is_empty() {
            tmp_alloc_serial.truncate(m);
        }
        // Unpack kind (bits 30-31) from tmp_class_ids and strip to clean class index.
        // Done after dedup/truncate so the extraction is over the final m unique objects.
        const CLASS_MASK: u32 = 0x3FFF_FFFF;
        let mut kind: Vec<u8> = Vec::with_capacity(m);
        for c in &mut tmp_class_ids {
            kind.push((*c >> 30) as u8);
            *c &= CLASS_MASK;
        }
        let class_ids = tmp_class_ids;
        let elem_count = tmp_elem_count;
        let alloc_stack_serial = tmp_alloc_serial;

        Ok(Pass1 {
            strings,
            class_map,
            class_serial_to_addr,
            id_map,
            class_ids,
            class_addr_table,
            alloc_stack_serial,
            kind,
            elem_count,
            gc_root_addrs,
            gc_root_types,
            thread_serial_to_obj_id,
            thread_local_pairs,
            stack_frames,
            stack_traces,
            stack_trace_thread,
            id_size,
            format,
            file_size,
            header_timestamp_ms,
            has_sticky_class_roots,
            instance_count,
            obj_array_count,
            prim_array_count,
            class_dump_count,
            utf8_records,
            load_class_records,
            unload_class_records,
            stack_frame_records,
            stack_trace_records,
            heap_dump_segments,
            gc_root_tag_counts,
        })
    }
}

/// Scans one HEAP_DUMP(_SEGMENT) body, appending one entry per object to the
/// parallel `tmp_*` arrays (kept 1:1 aligned) and collecting roots/threads.
#[allow(clippy::too_many_arguments)]
fn scan_heap_segment(
    r: &mut HprofReader,
    id_size: u8,
    mut remaining: u64,
    class_map: &mut HashMap<u64, ClassInfo>,
    tmp_addrs: &mut Vec<u64>,
    tmp_class_ids: &mut Vec<u32>,
    class_addr_table: &mut Vec<u64>,
    class_addr_to_idx: &mut HashMap<u64, u32>,
    tmp_elem_count: &mut Vec<u32>,
    tmp_alloc_serial: &mut Vec<u32>,
    gc_root_addrs: &mut Vec<u64>,
    gc_root_types: &mut Vec<u8>,
    thread_serial_to_obj_id: &mut HashMap<u32, u64>,
    thread_local_pairs: &mut Vec<ThreadLocalRoot>,
    has_sticky_class_roots: &mut bool,
    instance_count: &mut u64,
    obj_array_count: &mut u64,
    prim_array_count: &mut u64,
    class_dump_count: &mut u64,
    gc_root_tag_counts: &mut HashMap<u8, u64>,
) -> io::Result<()> {
    let ids = id_size as u64;

    while remaining > 0 {
        let sub_tag = r.u1()?;
        sub_remaining(&mut remaining, 1)?;

        match sub_tag {
            heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED => {
                *gc_root_tag_counts.entry(sub_tag).or_insert(0) += 1;
                gc_root_addrs.push(r.id()?);
                gc_root_types.push(sub_tag);
                sub_remaining(&mut remaining, ids)?;
            }
            heap::ROOT_JNI_GLOBAL => {
                *gc_root_tag_counts.entry(sub_tag).or_insert(0) += 1;
                gc_root_addrs.push(r.id()?);
                gc_root_types.push(sub_tag);
                r.skip(ids)?; // JNI global ref id
                sub_remaining(&mut remaining, 2 * ids)?;
            }
            heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME => {
                *gc_root_tag_counts.entry(sub_tag).or_insert(0) += 1;
                let local_id = r.id()?;
                let thread_serial = r.u4()?;
                let frame_number = r.u4()?;
                sub_remaining(&mut remaining, ids + 8)?;
                // NOT a direct GC root — synthetic edge from thread object (MAT parity)
                thread_local_pairs.push((thread_serial, frame_number, local_id));
            }
            heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                *gc_root_tag_counts.entry(sub_tag).or_insert(0) += 1;
                let local_id = r.id()?;
                let thread_serial = r.u4()?;
                sub_remaining(&mut remaining, ids + 4)?;
                // NOT a direct GC root — synthetic edge from thread object (MAT parity).
                // No stack-frame index in these records → sentinel u32::MAX.
                thread_local_pairs.push((thread_serial, u32::MAX, local_id));
            }
            heap::ROOT_STICKY_CLASS => {
                *gc_root_tag_counts.entry(sub_tag).or_insert(0) += 1;
                gc_root_addrs.push(r.id()?);
                gc_root_types.push(sub_tag);
                *has_sticky_class_roots = true;
                sub_remaining(&mut remaining, ids)?;
            }
            heap::ROOT_THREAD_OBJ => {
                *gc_root_tag_counts.entry(sub_tag).or_insert(0) += 1;
                let obj_id = r.id()?;
                let thread_serial = r.u4()?;
                r.skip(4)?; // stack_trace_serial
                sub_remaining(&mut remaining, ids + 8)?;
                gc_root_addrs.push(obj_id);
                gc_root_types.push(sub_tag);
                thread_serial_to_obj_id.insert(thread_serial, obj_id);
            }
            heap::CLASS_DUMP => {
                let (class_addr, consumed) = read_class_dump(r, id_size, class_map)?;
                sub_remaining(&mut remaining, consumed)?;
                *class_dump_count += 1;
                // Class objects must be in id_map so GC roots referencing them resolve correctly
                // (hprof-analyzer: scanClassDumpA1 calls state.appendAddress(classId))
                tmp_addrs.push(class_addr);
                {
                    let idx = *class_addr_to_idx.entry(class_addr).or_insert_with(|| {
                        let n = class_addr_table.len() as u32;
                        class_addr_table.push(class_addr);
                        n
                    });
                    tmp_class_ids.push(idx | (3u32 << 30)); // class-of-class resolved later in pass2
                }
                tmp_elem_count.push(0);
                // CLASS_DUMP has no per-object alloc serial; push 0 so the array
                // stays 1:1 with the object ordering.
                tmp_alloc_serial.push(0);
            }
            heap::INSTANCE_DUMP => {
                let addr = r.id()?;
                let stack_serial = r.u4()?; // stack_trace_serial(u4)
                tmp_alloc_serial.push(stack_serial);
                let class_id = r.id()?;
                let data_len = r.u4()? as u64;
                r.skip(data_len)?;
                tmp_addrs.push(addr);
                {
                    let idx = *class_addr_to_idx.entry(class_id).or_insert_with(|| {
                        let n = class_addr_table.len() as u32;
                        class_addr_table.push(class_id);
                        n
                    });
                    tmp_class_ids.push(idx); // kind=0, bits 30-31 = 0
                }
                tmp_elem_count.push(0);
                sub_remaining(&mut remaining, ids + 4 + ids + 4 + data_len)?;
                *instance_count += 1;
            }
            heap::OBJ_ARRAY_DUMP => {
                // addr(id) + stack_serial(u4) + count(u4) + elem_class(id)
                // + count element ids.
                let addr = r.id()?;
                let stack_serial = r.u4()?; // stack_trace_serial
                tmp_alloc_serial.push(stack_serial);
                let count = r.u4()? as u64;
                let elem_class_id = r.id()?;
                let byte_len = count.saturating_mul(ids);
                r.skip(byte_len)?;
                tmp_addrs.push(addr);
                {
                    let idx = *class_addr_to_idx.entry(elem_class_id).or_insert_with(|| {
                        let n = class_addr_table.len() as u32;
                        class_addr_table.push(elem_class_id);
                        n
                    });
                    tmp_class_ids.push(idx | (1u32 << 30)); // kind=1 object array
                }
                tmp_elem_count.push(count as u32);
                sub_remaining(&mut remaining, ids + 4 + 4 + ids + byte_len)?;
                *obj_array_count += 1;
            }
            heap::PRIM_ARRAY_DUMP => {
                // addr(id) + stack_serial(u4) + count(u4) + elem_type(u1)
                // + count*elem_size raw element bytes.
                let addr = r.id()?;
                let stack_serial = r.u4()?; // stack_trace_serial
                tmp_alloc_serial.push(stack_serial);
                let count = r.u4()? as u64;
                let elem_type_code = r.u1()?;
                let elem_size = HprofType::from_code(elem_type_code)
                    .map(|t| t.byte_size() as u64)
                    .unwrap_or(1);
                let byte_len = count.saturating_mul(elem_size);
                r.skip(byte_len)?;
                tmp_addrs.push(addr);
                // kind 2 stores the raw element type code (0-11) in class_ids bits 0-29,
                // with kind=2 packed into bits 30-31.
                tmp_class_ids.push((2u32 << 30) | elem_type_code as u32);
                tmp_elem_count.push(count as u32);
                sub_remaining(&mut remaining, ids + 4 + 4 + 1 + byte_len)?;
                *prim_array_count += 1;
            }
            heap::HEAP_DUMP_INFO => {
                // u4 heap_id + id heap_name_string_id. No object/class payload;
                // skip it so the sub-record stream stays aligned.
                r.skip(4 + ids)?;
                sub_remaining(&mut remaining, 4 + ids)?;
            }
            other => {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("unknown heap sub-tag: 0x{other:02x}, remaining={remaining}"),
                ));
            }
        }
    }
    Ok(())
}

/// Reads a CLASS_DUMP sub-record. Returns exact byte count consumed.
fn read_class_dump(
    r: &mut HprofReader,
    id_size: u8,
    class_map: &mut HashMap<u64, ClassInfo>,
) -> io::Result<(u64, u64)> {
    // (class_addr, consumed)
    let ids = id_size as u64;
    let mut consumed: u64 = 0;

    let class_addr = r.id()?;
    consumed += ids;
    r.skip(4)?;
    consumed += 4; // stack_trace_serial
    let super_id = r.id()?;
    consumed += ids;
    let loader_id = r.id()?;
    consumed += ids;
    r.skip(ids)?;
    consumed += ids; // signers_id
    r.skip(ids)?;
    consumed += ids; // protection_domain_id
    r.skip(ids)?;
    consumed += ids; // reserved1
    r.skip(ids)?;
    consumed += ids; // reserved2
    let instance_size = r.u4()?;
    consumed += 4;

    // Constant pool entries
    let cp_count = r.u2()? as u64;
    consumed += 2;
    for _ in 0..cp_count {
        r.skip(2)?;
        consumed += 2; // cp index (u2)
        let cp_type = r.u1()?;
        consumed += 1;
        let vs = value_size(cp_type, id_size);
        r.skip(vs)?;
        consumed += vs;
    }

    // Static fields
    let static_count = r.u2()? as u64;
    consumed += 2;
    let mut static_obj_count = 0u32;
    let mut static_prim_bytes = 0u32;
    for _ in 0..static_count {
        r.skip(ids)?;
        consumed += ids; // name_id
        let field_type = r.u1()?;
        consumed += 1;
        let vs = value_size(field_type, id_size);
        r.skip(vs)?;
        consumed += vs;
        if field_type == 2 {
            static_obj_count += 1;
        } else {
            static_prim_bytes += vs as u32;
        }
    }

    // Instance fields
    let field_count = r.u2()? as u64;
    consumed += 2;
    let mut fields = Vec::with_capacity(field_count as usize);
    for _ in 0..field_count {
        let name_id = r.id()?;
        consumed += ids;
        let type_code = r.u1()?;
        consumed += 1;
        let htype = HprofType::from_code(type_code).unwrap_or(HprofType::Int);
        fields.push((name_id, htype));
    }

    let entry = class_map.entry(class_addr).or_default();
    entry.super_id = super_id;
    entry.loader_id = loader_id;
    entry.instance_size = instance_size;
    entry.fields = fields;
    entry.static_obj_count = static_obj_count;
    entry.static_prim_bytes = static_prim_bytes;

    Ok((class_addr, consumed))
}

/// Byte width of a static/constant-pool field value: object refs are `id_size`,
/// primitives their type width, unknown codes 0.
fn value_size(type_code: u8, id_size: u8) -> u64 {
    match HprofType::from_code(type_code) {
        Some(HprofType::Object) => id_size as u64,
        Some(t) => t.byte_size() as u64,
        None => 0,
    }
}

/// Apply the gather permutation `perm` in place: after the call, position `k`
/// holds the element that was originally at `perm[k]` (i.e. equivalent to
/// `out[k] = src[perm[k]]` but with no output allocation). `swap(a, b)` must
/// exchange element `a` with element `b` in every parallel array being
/// permuted. `perm` is consumed as scratch — its top bit (1<<31) is used as a
/// per-slot "placed" marker, so entries must be < 2^31 (heaps hold < 4G
/// objects). On return `perm` is left with all top bits set (caller drops it).
fn apply_permutation_in_place(perm: &mut [u32], mut swap: impl FnMut(usize, usize)) {
    const PLACED: u32 = 1 << 31;
    let n = perm.len();
    for start in 0..n {
        if perm[start] & PLACED != 0 {
            continue;
        }
        // Walk the cycle. `hole` is the slot currently waiting to receive its
        // final element; we pull from `src = perm[hole]` until we return to
        // `start`, marking each slot placed as we fix it.
        let mut hole = start;
        loop {
            let src = (perm[hole] & !PLACED) as usize;
            perm[hole] |= PLACED;
            if src == start {
                break;
            }
            swap(hole, src);
            hole = src;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Naive gather reference: out[k] = src[perm[k]].
    fn gather<T: Copy>(src: &[T], perm: &[u32]) -> Vec<T> {
        perm.iter().map(|&p| src[p as usize]).collect()
    }

    #[test]
    fn apply_permutation_matches_gather_multicycle() {
        // A permutation with a 3-cycle (0->2->4->0), a 2-cycle (1<->3),
        // and a fixed point (5).
        let perm: [u32; 6] = [2, 3, 4, 1, 0, 5];
        let a: [u64; 6] = [10, 11, 12, 13, 14, 15];
        let b: [u32; 6] = [100, 101, 102, 103, 104, 105];
        let c: [u8; 6] = [0, 1, 2, 3, 4, 5];
        let want_a = gather(&a, &perm);
        let want_b = gather(&b, &perm);
        let want_c = gather(&c, &perm);

        let mut pa = a;
        let mut pb = b;
        let mut pc = c;
        let mut work = perm;
        apply_permutation_in_place(&mut work, |x, y| {
            pa.swap(x, y);
            pb.swap(x, y);
            pc.swap(x, y);
        });
        assert_eq!(pa.to_vec(), want_a, "u64 array");
        assert_eq!(pb.to_vec(), want_b, "u32 array");
        assert_eq!(pc.to_vec(), want_c, "u8 array");
    }

    #[test]
    fn apply_permutation_identity_and_reverse() {
        // Identity.
        let id: [u32; 4] = [0, 1, 2, 3];
        let a: [u64; 4] = [7, 8, 9, 10];
        let mut pa = a;
        let mut w = id;
        apply_permutation_in_place(&mut w, |x, y| pa.swap(x, y));
        assert_eq!(pa, a);

        // Full reverse (two 2-cycles): out[k]=src[3-k].
        let rev: [u32; 4] = [3, 2, 1, 0];
        let b: [u64; 4] = [7, 8, 9, 10];
        let want = gather(&b, &rev);
        let mut pb = b;
        let mut w2 = rev;
        apply_permutation_in_place(&mut w2, |x, y| pb.swap(x, y));
        assert_eq!(pb.to_vec(), want);
    }

    use proptest::prelude::*;

    // Generate a random permutation of [0, n) for a random n in [1, 255):
    // start from the identity and shuffle it. `prop_shuffle` requires the
    // `Strategy` trait (in the prelude).
    fn arb_permutation() -> impl Strategy<Value = Vec<u32>> {
        (1usize..256).prop_flat_map(|n| Just((0..n as u32).collect::<Vec<u32>>()).prop_shuffle())
    }

    proptest! {
        // Apply the in-place permutation across two independent payload arrays
        // in lockstep and assert each equals the naive gather out[k]=src[perm[k]].
        // Shrinking pins any direction/off-by-one bug to a minimal
        // counterexample. This is the correctness backbone for the pass1
        // order-elimination (silent swap-discipline bugs corrupt payloads and
        // break big-dump bit-exactness only at scale).
        #[test]
        fn apply_permutation_equals_gather(perm in arb_permutation()) {
            let n = perm.len();
            // Deterministic-from-index payloads so the check is self-contained.
            let a: Vec<u64> = (0..n as u64).map(|i| i.wrapping_mul(0x9E37_79B9)).collect();
            let b: Vec<u32> = (0..n as u32).map(|i| i.wrapping_mul(2_654_435_761)).collect();
            let want_a = gather(&a, &perm);
            let want_b = gather(&b, &perm);

            let mut pa = a;
            let mut pb = b;
            let mut work = perm.clone();
            apply_permutation_in_place(&mut work, |x, y| {
                pa.swap(x, y);
                pb.swap(x, y);
            });
            prop_assert_eq!(&pa, &want_a);
            prop_assert_eq!(&pb, &want_b);
            // Every slot must be marked placed exactly once (top bit set).
            for &w in &work {
                prop_assert_ne!(w & (1u32 << 31), 0);
            }
        }
    }

    /// Standalone mirror of the pass1 finalize flow (sort-by-address ->
    /// permute-in-place -> sequential dedup+compact), operating on two payload
    /// arrays. Returns (unique_sorted_addrs, compacted_payload_a,
    /// compacted_payload_b). This is exactly the mechanism in `Pass1::run`,
    /// extracted so it can be property-tested against a naive reference.
    fn finalize_flow(
        addrs: &[u64],
        pa_in: &[u64],
        pb_in: &[u32],
    ) -> (Vec<u64>, Vec<u64>, Vec<u32>) {
        let n = addrs.len();
        let mut a = addrs.to_vec();
        let mut pa = pa_in.to_vec();
        let mut pb = pb_in.to_vec();
        let mut order: Vec<u32> = (0..n as u32).collect();
        order.sort_unstable_by_key(|&i| a[i as usize]);
        apply_permutation_in_place(&mut order, |x, y| {
            a.swap(x, y);
            pa.swap(x, y);
            pb.swap(x, y);
        });
        drop(order);
        let mut out_addr = Vec::new();
        let mut prev = u64::MAX;
        let mut write = 0usize;
        for rank in 0..n {
            let addr = a[rank];
            if addr != prev {
                out_addr.push(addr);
                if write != rank {
                    pa[write] = pa[rank];
                    pb[write] = pb[rank];
                }
                write += 1;
                prev = addr;
            }
        }
        pa.truncate(write);
        pb.truncate(write);
        (out_addr, pa, pb)
    }

    proptest! {
        // Full finalize flow == naive reference. We use a BTreeMap keyed by
        // address so each distinct address carries ONE payload pair (matching
        // reality: an object address has a single payload). The reference is
        // the address-sorted map: keys are the unique sorted addrs, values are
        // the payloads. This pins the composite (sort+permute+dedup+compact)
        // that the big dump actually exercises, not just the raw permutation.
        #[test]
        fn finalize_flow_matches_btreemap_reference(
            pairs in proptest::collection::vec(
                (any::<u64>(), any::<u64>(), any::<u32>()),
                0..300,
            )
        ) {
            use std::collections::BTreeMap;
            // Deduplicate inputs by address so payloads are well-defined; keep
            // the LAST write per address for the input arrays (arbitrary — the
            // reference below reads the same map, so it stays consistent).
            let mut map: BTreeMap<u64, (u64, u32)> = BTreeMap::new();
            for &(addr, va, vb) in &pairs {
                map.insert(addr, (va, vb));
            }
            let addrs: Vec<u64> = map.keys().copied().collect();
            let pa: Vec<u64> = addrs.iter().map(|k| map[k].0).collect();
            let pb: Vec<u32> = addrs.iter().map(|k| map[k].1).collect();

            let (out_addr, out_a, out_b) = finalize_flow(&addrs, &pa, &pb);

            // Reference: unique addrs sorted ascending, payloads follow.
            let want_addr: Vec<u64> = map.keys().copied().collect();
            let want_a: Vec<u64> = map.values().map(|v| v.0).collect();
            let want_b: Vec<u32> = map.values().map(|v| v.1).collect();

            prop_assert_eq!(&out_addr, &want_addr, "addresses");
            prop_assert_eq!(&out_a, &want_a, "payload a");
            prop_assert_eq!(&out_b, &want_b, "payload b");
            // Strictly ascending, no duplicates.
            for w in out_addr.windows(2) {
                prop_assert!(w[0] < w[1], "addrs must be strictly ascending");
            }
        }

        // Finalize flow correctly collapses duplicate addresses even when the
        // raw input contains many repeats in arbitrary order. Payload per
        // address is derived FROM the address (deterministic) so which
        // duplicate the unstable sort keeps does not matter.
        #[test]
        fn finalize_flow_collapses_duplicates(
            raw in proptest::collection::vec(0u64..32, 0..400)
        ) {
            let addrs: Vec<u64> = raw.clone();
            // payload is a pure function of the address -> all duplicates agree.
            let pa: Vec<u64> = addrs.iter().map(|&x| x.wrapping_mul(0x9E37_79B9)).collect();
            let pb: Vec<u32> = addrs.iter().map(|&x| (x as u32).wrapping_mul(2_654_435_761)).collect();

            let (out_addr, out_a, out_b) = finalize_flow(&addrs, &pa, &pb);

            let mut want_addr: Vec<u64> = addrs.clone();
            want_addr.sort_unstable();
            want_addr.dedup();
            let want_a: Vec<u64> = want_addr.iter().map(|&x| x.wrapping_mul(0x9E37_79B9)).collect();
            let want_b: Vec<u32> = want_addr.iter().map(|&x| (x as u32).wrapping_mul(2_654_435_761)).collect();

            prop_assert_eq!(&out_addr, &want_addr, "addresses");
            prop_assert_eq!(&out_a, &want_a, "payload a");
            prop_assert_eq!(&out_b, &want_b, "payload b");
        }
    }

    #[test]
    fn finalize_flow_empty() {
        let (a, pa, pb) = finalize_flow(&[], &[], &[]);
        assert!(a.is_empty() && pa.is_empty() && pb.is_empty());
    }

    #[test]
    fn finalize_flow_all_same_address() {
        let addrs = [7u64, 7, 7, 7];
        let pa = [10u64, 20, 30, 40];
        let pb = [1u32, 2, 3, 4];
        let (a, ra, rb) = finalize_flow(&addrs, &pa, &pb);
        assert_eq!(a, vec![7]);
        assert_eq!(ra.len(), 1);
        assert_eq!(rb.len(), 1);
        // Kept payload must be one of the inputs.
        assert!(pa.contains(&ra[0]) && pb.contains(&rb[0]));
    }

    const DUMP: &str = "tests/fixtures/dump_0_fj-kmeans.hprof";

    // Ground truth from the hprof-analyzer `diagnose` command on dump_0_fj-kmeans.
    const EXPECTED_INSTANCES: u64 = 2_698_510;
    const EXPECTED_OBJ_ARRAYS: u64 = 504_353;
    const EXPECTED_PRIM_ARRAYS: u64 = 27_379;
    const EXPECTED_CLASSES: u64 = 2_646;
    const EXPECTED_GC_ROOTS: u64 = 1_454 + 135 + 101; // 1,690 (JAVA_FRAME/JNI_LOCAL/etc -> thread_local_pairs)

    #[test]
    fn record_counts_match_diagnose() {
        if !std::path::Path::new(DUMP).exists() {
            return;
        }
        let p = Pass1::run(DUMP).unwrap();
        assert_eq!(p.instance_count, EXPECTED_INSTANCES, "instances");
        assert_eq!(p.obj_array_count, EXPECTED_OBJ_ARRAYS, "obj arrays");
        assert_eq!(p.prim_array_count, EXPECTED_PRIM_ARRAYS, "prim arrays");
        assert_eq!(p.class_dump_count, EXPECTED_CLASSES, "classes");
        assert_eq!(
            p.gc_root_addrs.len() as u64,
            EXPECTED_GC_ROOTS,
            "gc roots (got {})",
            p.gc_root_addrs.len()
        );

        // Record census: per-object dump counters must mirror the validation
        // counters exactly (same records, counted twice for cross-check).
        assert_eq!(p.instance_count, EXPECTED_INSTANCES, "census instances");
        assert_eq!(p.class_dump_count, EXPECTED_CLASSES, "census classes");
        // These record types are always present in a HotSpot dump.
        assert!(p.utf8_records > 0, "utf8 records populated");
        assert!(p.load_class_records > 0, "load_class records populated");
        assert!(p.stack_trace_records > 0, "stack_trace records populated");
        assert!(p.heap_dump_segments > 0, "heap dump segments populated");
        // LOAD_CLASS is emitted once per loaded class, so it must cover at
        // least every CLASS_DUMP we saw.
        assert!(
            p.load_class_records >= p.class_dump_count,
            "load_class ({}) >= class_dump ({})",
            p.load_class_records,
            p.class_dump_count
        );
        // The per-GC-root-tag census must total to every root sub-record seen,
        // which equals the direct GC roots plus the thread-local synthetic ones.
        let census_root_total: u64 = p.gc_root_tag_counts.values().sum();
        assert_eq!(
            census_root_total,
            p.gc_root_addrs.len() as u64 + p.thread_local_pairs.len() as u64,
            "gc_root_tag_counts total"
        );
    }

    #[test]
    fn total_object_count() {
        if !std::path::Path::new(DUMP).exists() {
            return;
        }
        let p = Pass1::run(DUMP).unwrap();
        // id_map now includes class objects (from CLASS_DUMP records) in addition to instances/arrays
        let expected =
            EXPECTED_INSTANCES + EXPECTED_OBJ_ARRAYS + EXPECTED_PRIM_ARRAYS + p.class_dump_count;
        assert_eq!(
            p.id_map.len() as u64,
            expected,
            "total objects (got {})",
            p.id_map.len()
        );
    }

    #[test]
    fn parallel_arrays_consistent() {
        if !std::path::Path::new(DUMP).exists() {
            return;
        }
        let p = Pass1::run(DUMP).unwrap();
        assert_eq!(p.id_map.len(), p.class_ids.len(), "class_ids len");
    }

    #[test]
    fn header_fields() {
        if !std::path::Path::new(DUMP).exists() {
            return;
        }
        let p = Pass1::run(DUMP).unwrap();
        assert_eq!(p.id_size, 8);
        assert!(p.format.starts_with("JAVA PROFILE"));
        assert!(p.has_sticky_class_roots);
        assert!(!p.strings.is_empty());
    }

    #[test]
    fn class_name_resolvable() {
        if !std::path::Path::new(DUMP).exists() {
            return;
        }
        let p = Pass1::run(DUMP).unwrap();
        // Every class in class_map should have a name_id, and that name_id should be in strings
        let mut resolved = 0usize;
        for ci in p.class_map.values() {
            if p.strings.contains_key(&ci.name_id) {
                resolved += 1;
            }
        }
        // At least 90% of classes should have resolvable names
        let pct = resolved * 100 / p.class_map.len();
        assert!(pct >= 90, "only {pct}% of classes have resolvable names");
    }
}
