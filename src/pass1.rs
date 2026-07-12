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
        // objects, so u32 suffices and halves this 90 MB->45 MB scaffold).
        let n = tmp_addrs.len();
        let mut order: Vec<u32> = (0..n as u32).collect();
        crate::trace::probe("pass1: before order.sort_unstable_by_key");
        order.sort_unstable_by_key(|&i| tmp_addrs[i as usize]);
        crate::trace::probe("pass1: after order.sort_unstable_by_key");

        // Build id_map while compacting `order` in place to keep only the
        // first `order` entry at each distinct address (dedup). No separate
        // keep-mask (was a full Vec<bool>, ~0.5 GB @514M): we overwrite
        // order[write] <= order[rank] as we go, then truncate to m. The
        // gathers below then iterate the compacted order with no per-rank
        // branch.
        // `order` is sorted by address, so we feed strictly-ascending, deduped
        // addresses directly into the two-level index — no 4.1GB staging Vec.
        let mut id_map = IdMap::new();
        id_map.reserve_offsets(n);
        let mut prev_addr = u64::MAX;
        let mut write = 0usize;
        for rank in 0..order.len() {
            let i = order[rank];
            let a = tmp_addrs[i as usize];
            if a != prev_addr {
                id_map.push_sorted_addr(a);
                order[write] = i;
                write += 1;
                prev_addr = a;
            }
        }
        crate::trace::probe("pass1: before id_map.finalize (tmp_addrs+order+tmps live)");
        order.truncate(write);
        id_map.finalize_sorted();
        crate::trace::probe("pass1: after id_map.finalize (offsets built)");
        let m = id_map.len();
        drop(tmp_addrs);
        crate::trace::probe("pass1: after drop(tmp_addrs)");

        // Gather each parallel array in its own pass, then free the
        // source buffer immediately — only one tmp/output pair is
        // resident at a time, trimming the pass1 transient peak. `order`
        // is now the compacted (unique, sorted) index list of length m.
        let mut class_ids: Vec<u32> = Vec::with_capacity(m);
        for &i in &order {
            class_ids.push(tmp_class_ids[i as usize]);
        }
        drop(tmp_class_ids);

        let mut shallow_sizes: Vec<u32> = Vec::with_capacity(m);
        for &i in &order {
            shallow_sizes.push(tmp_shallow[i as usize]);
        }
        drop(tmp_shallow);

        let mut kind: Vec<u8> = Vec::with_capacity(m);
        for &i in &order {
            kind.push(tmp_kind[i as usize]);
        }
        drop(tmp_kind);

        let mut elem_count: Vec<u32> = Vec::with_capacity(m);
        for &i in &order {
            elem_count.push(tmp_elem_count[i as usize]);
        }
        drop(tmp_elem_count);

        drop(order);

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

#[cfg(test)]
mod tests {
    use super::*;

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
