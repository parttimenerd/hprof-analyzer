use std::{
    collections::HashMap,
    io::{self, ErrorKind},
};

use crate::{
    id_map::IdMap,
    reader::HprofReader,
    types::{HprofType, heap, tags},
};

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

pub struct Pass1 {
    pub strings: HashMap<u64, String>,
    pub class_map: HashMap<u64, ClassInfo>,
    #[allow(dead_code)]
    pub class_serial_to_addr: HashMap<u32, u64>,
    pub id_map: IdMap,
    /// Per-object class reference, u32-interned to halve this array (was
    /// Vec<u64> class addresses @514M = 4.1GB). For kind 0/1/3 it is an index
    /// into class_addr_table (the distinct class-object addresses); for kind 2
    /// (primitive array) it is the raw element type code (0-11).
    pub class_ids: Vec<u32>,
    /// Distinct class-object addresses; class_ids[i] indexes this for kind 0/1/3.
    pub class_addr_table: Vec<u64>,
    pub shallow_sizes: Vec<u32>,
    /// Per-object kind: 0=instance, 1=obj_array, 2=prim_array, 3=class_obj
    pub kind: Vec<u8>,
    /// Per-object raw element count (arrays only; 0 otherwise)
    pub elem_count: Vec<u32>,
    pub gc_root_addrs: Vec<u64>,
    #[allow(dead_code)]
    pub gc_root_types: Vec<u8>,
    /// threadSerial → thread object address (from ROOT_THREAD_OBJ records)
    pub thread_serial_to_obj_id: HashMap<u32, u64>,
    /// (threadSerial, localAddr) from JAVA_FRAME/JNI_LOCAL/NATIVE_STACK/THREAD_BLOCK
    pub thread_local_pairs: Vec<(u32, u64)>,
    pub id_size: u8,
    pub format: String,
    pub file_size: u64,
    pub has_sticky_class_roots: bool,
    // Validation counters
    pub instance_count: u64,
    pub obj_array_count: u64,
    pub prim_array_count: u64,
    pub class_dump_count: u64,
}

impl Pass1 {
    pub fn run(path: &str) -> io::Result<Self> {
        let file_size = std::fs::metadata(path)?.len();
        let mut r = HprofReader::open(path)?;
        let id_size = r.id_size;
        let format = r.format.clone();

        let mut strings: HashMap<u64, String> = HashMap::new();
        let mut class_map: HashMap<u64, ClassInfo> = HashMap::new();
        let mut class_serial_to_addr: HashMap<u32, u64> = HashMap::new();
        let mut tmp_addrs: Vec<u64> = Vec::new();
        let mut tmp_class_ids: Vec<u32> = Vec::new();
        // Intern class addresses to u32 during scan (heaps have < 4G classes;
        // this dump ~133K). kind 2 stores the raw type code instead.
        let mut class_addr_table: Vec<u64> = Vec::new();
        let mut class_addr_to_idx: HashMap<u64, u32> = HashMap::new();
        let mut tmp_shallow: Vec<u32> = Vec::new();
        let mut tmp_kind: Vec<u8> = Vec::new();
        let mut tmp_elem_count: Vec<u32> = Vec::new();
        let mut gc_root_addrs: Vec<u64> = Vec::new();
        let mut gc_root_types: Vec<u8> = Vec::new();
        let mut thread_serial_to_obj_id: HashMap<u32, u64> = HashMap::new();
        let mut thread_local_pairs: Vec<(u32, u64)> = Vec::new();
        let mut has_sticky_class_roots = false;
        let mut instance_count = 0u64;
        let mut obj_array_count = 0u64;
        let mut prim_array_count = 0u64;
        let mut class_dump_count = 0u64;

        loop {
            let tag = match r.u1() {
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
                other => other?,
            };
            let _timestamp = r.u4()?;
            let length = r.u4()? as u64;

            match tag {
                tags::STRING_IN_UTF8 => {
                    let str_id = r.id()?;
                    let bytes = r.read_bytes((length - id_size as u64) as usize)?;
                    strings.insert(str_id, String::from_utf8_lossy(&bytes).into_owned());
                }
                tags::LOAD_CLASS => {
                    // serial(4) + class_addr(id) + stack_serial(4) + name_id(id)
                    let serial = r.u4()?;
                    let class_addr = r.id()?;
                    let _stack_serial = r.u4()?;
                    let name_id = r.id()?;
                    class_serial_to_addr.insert(serial, class_addr);
                    class_map.entry(class_addr).or_default().name_id = name_id;
                }
                tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                    scan_heap_segment(
                        &mut r,
                        id_size,
                        length,
                        &mut class_map,
                        &mut tmp_addrs,
                        &mut tmp_class_ids,
                        &mut class_addr_table,
                        &mut class_addr_to_idx,
                        &mut tmp_shallow,
                        &mut tmp_kind,
                        &mut tmp_elem_count,
                        &mut gc_root_addrs,
                        &mut gc_root_types,
                        &mut thread_serial_to_obj_id,
                        &mut thread_local_pairs,
                        &mut has_sticky_class_roots,
                        &mut instance_count,
                        &mut obj_array_count,
                        &mut prim_array_count,
                        &mut class_dump_count,
                    )?;
                }
                tags::HEAP_DUMP_END => break,
                _ => {
                    r.skip(length)?;
                }
            }
        }

        crate::trace::probe("pass1: after scan loop (all tmp_* grown)");
        // Fix up shallow sizes where class wasn't yet seen at scan time.
        // tmp_class_ids holds interned indices; resolve to addr for kinds that
        // reference a class (0=instance, 3=class-obj). kind 1/2 (arrays) skip.
        for (i, &cidx) in tmp_class_ids.iter().enumerate() {
            if tmp_shallow[i] == 0 && (tmp_kind[i] == 0 || tmp_kind[i] == 3) {
                let addr = class_addr_table[cidx as usize];
                if let Some(ci) = class_map.get(&addr) {
                    tmp_shallow[i] = ci.instance_size;
                }
            }
        }

        // Sort by address and deduplicate (same address may appear under
        // multiple roots). `order` is a u32 permutation (heaps hold < 4 G
        // objects, so u32 suffices). To keep `order` (2 GB @514M) off the
        // binding pass1 peak, we apply it IN PLACE to every parallel array so
        // the arrays themselves become address-sorted, then DROP `order`
        // before building the id_map offsets and deduping — so `order`,
        // `tmp_addrs`, and `id_map.offsets` never coexist (that 3-way overlap
        // was the ~14.9 GB peak).
        let n = tmp_addrs.len();
        let mut order: Vec<u32> = (0..n as u32).collect();
        crate::trace::probe("pass1: before order.sort_unstable_by_key");
        order.sort_unstable_by_key(|&i| tmp_addrs[i as usize]);
        crate::trace::probe("pass1: after order.sort_unstable_by_key");

        // Permute all five parallel arrays into address-sorted order in place
        // (no output allocation). `order` is consumed as scratch (top-bit
        // marker) and dropped immediately after — freeing 2 GB before the
        // dedup/offsets pass below.
        apply_permutation_in_place(&mut order, |x, y| {
            tmp_addrs.swap(x, y);
            tmp_class_ids.swap(x, y);
            tmp_shallow.swap(x, y);
            tmp_kind.swap(x, y);
            tmp_elem_count.swap(x, y);
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
                    tmp_class_ids[write] = tmp_class_ids[rank];
                    tmp_shallow[write] = tmp_shallow[rank];
                    tmp_kind[write] = tmp_kind[rank];
                    tmp_elem_count[write] = tmp_elem_count[rank];
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
        tmp_shallow.truncate(m);
        tmp_kind.truncate(m);
        tmp_elem_count.truncate(m);
        let class_ids = tmp_class_ids;
        let shallow_sizes = tmp_shallow;
        let kind = tmp_kind;
        let elem_count = tmp_elem_count;

        Ok(Pass1 {
            strings,
            class_map,
            class_serial_to_addr,
            id_map,
            class_ids,
            class_addr_table,
            shallow_sizes,
            kind,
            elem_count,
            gc_root_addrs,
            gc_root_types,
            thread_serial_to_obj_id,
            thread_local_pairs,
            id_size,
            format,
            file_size,
            has_sticky_class_roots,
            instance_count,
            obj_array_count,
            prim_array_count,
            class_dump_count,
        })
    }
}

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
    tmp_shallow: &mut Vec<u32>,
    tmp_kind: &mut Vec<u8>,
    tmp_elem_count: &mut Vec<u32>,
    gc_root_addrs: &mut Vec<u64>,
    gc_root_types: &mut Vec<u8>,
    thread_serial_to_obj_id: &mut HashMap<u32, u64>,
    thread_local_pairs: &mut Vec<(u32, u64)>,
    has_sticky_class_roots: &mut bool,
    instance_count: &mut u64,
    obj_array_count: &mut u64,
    prim_array_count: &mut u64,
    class_dump_count: &mut u64,
) -> io::Result<()> {
    let ids = id_size as u64;

    while remaining > 0 {
        let sub_tag = r.u1()?;
        remaining -= 1;

        match sub_tag {
            heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED => {
                gc_root_addrs.push(r.id()?);
                gc_root_types.push(sub_tag);
                remaining -= ids;
            }
            heap::ROOT_JNI_GLOBAL => {
                gc_root_addrs.push(r.id()?);
                gc_root_types.push(sub_tag);
                r.skip(ids)?; // JNI global ref id
                remaining -= 2 * ids;
            }
            heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME => {
                let local_id = r.id()?;
                let thread_serial = r.u4()?;
                r.skip(4)?; // frame_number
                remaining -= ids + 8;
                // NOT a direct GC root — synthetic edge from thread object (MAT parity)
                thread_local_pairs.push((thread_serial, local_id));
            }
            heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                let local_id = r.id()?;
                let thread_serial = r.u4()?;
                remaining -= ids + 4;
                // NOT a direct GC root — synthetic edge from thread object (MAT parity)
                thread_local_pairs.push((thread_serial, local_id));
            }
            heap::ROOT_STICKY_CLASS => {
                gc_root_addrs.push(r.id()?);
                gc_root_types.push(sub_tag);
                *has_sticky_class_roots = true;
                remaining -= ids;
            }
            heap::ROOT_THREAD_OBJ => {
                let obj_id = r.id()?;
                let thread_serial = r.u4()?;
                r.skip(4)?; // stack_trace_serial
                remaining -= ids + 8;
                gc_root_addrs.push(obj_id);
                gc_root_types.push(sub_tag);
                thread_serial_to_obj_id.insert(thread_serial, obj_id);
            }
            heap::CLASS_DUMP => {
                let (class_addr, consumed) = read_class_dump(r, id_size, class_map)?;
                remaining -= consumed;
                *class_dump_count += 1;
                // Class objects must be in id_map so GC roots referencing them resolve correctly
                // (hprof-redact: scanClassDumpA1 calls state.appendAddress(classId))
                tmp_addrs.push(class_addr);
                {
                    let idx = *class_addr_to_idx.entry(class_addr).or_insert_with(|| {
                        let n = class_addr_table.len() as u32;
                        class_addr_table.push(class_addr);
                        n
                    });
                    tmp_class_ids.push(idx); // class-of-class resolved later in pass2
                }
                tmp_shallow.push(0); // pass2 recalculates shallow size for class objects
                tmp_kind.push(3);
                tmp_elem_count.push(0);
            }
            heap::INSTANCE_DUMP => {
                let addr = r.id()?;
                r.skip(4)?; // stack_trace_serial(u4)
                let class_id = r.id()?;
                let data_len = r.u4()? as u64;
                r.skip(data_len)?;
                let shallow = class_map
                    .get(&class_id)
                    .map(|c| c.instance_size)
                    .unwrap_or(0);
                tmp_addrs.push(addr);
                {
                    let idx = *class_addr_to_idx.entry(class_id).or_insert_with(|| {
                        let n = class_addr_table.len() as u32;
                        class_addr_table.push(class_id);
                        n
                    });
                    tmp_class_ids.push(idx);
                }
                tmp_shallow.push(shallow);
                tmp_kind.push(0);
                tmp_elem_count.push(0);
                remaining -= ids + 4 + ids + 4 + data_len;
                *instance_count += 1;
            }
            heap::OBJ_ARRAY_DUMP => {
                let addr = r.id()?;
                r.skip(4)?; // stack_trace_serial
                let count = r.u4()? as u64;
                let elem_class_id = r.id()?;
                r.skip(count * ids)?;
                // shallow size: use file id_size for elements (exact formula in pass2/report)
                let shallow = (ids + ids + 4 + 4 + count * ids) as u32;
                tmp_addrs.push(addr);
                {
                    let idx = *class_addr_to_idx.entry(elem_class_id).or_insert_with(|| {
                        let n = class_addr_table.len() as u32;
                        class_addr_table.push(elem_class_id);
                        n
                    });
                    tmp_class_ids.push(idx);
                }
                tmp_shallow.push(shallow);
                tmp_kind.push(1);
                tmp_elem_count.push(count as u32);
                remaining -= ids + 4 + 4 + ids + count * ids;
                *obj_array_count += 1;
            }
            heap::PRIM_ARRAY_DUMP => {
                let addr = r.id()?;
                r.skip(4)?; // stack_trace_serial
                let count = r.u4()? as u64;
                let elem_type_code = r.u1()?;
                let elem_size = HprofType::from_code(elem_type_code)
                    .map(|t| t.byte_size() as u64)
                    .unwrap_or(1);
                r.skip(count * elem_size)?;
                let shallow = (ids + ids + 4 + 4 + 1 + count * elem_size) as u32;
                tmp_addrs.push(addr);
                tmp_class_ids.push(elem_type_code as u32);
                tmp_shallow.push(shallow);
                tmp_kind.push(2);
                tmp_elem_count.push(count as u32);
                remaining -= ids + 4 + 4 + 1 + count * elem_size;
                *prim_array_count += 1;
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

    const DUMP: &str = "/home/i560383/test-heapdumps/dump_0_fj-kmeans.hprof";

    // Ground truth from: java -jar ~/hprof-redact.jar diagnose dump_0_fj-kmeans.hprof
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
        assert_eq!(p.id_map.len(), p.shallow_sizes.len(), "shallow_sizes len");
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
