use std::{
    collections::HashMap,
    io::{self, ErrorKind},
};

use crate::{
    id_map::IdMap,
    reader::HprofReader,
    types::{heap, tags, HprofType},
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
    pub class_serial_to_addr: HashMap<u32, u64>,
    pub id_map: IdMap,
    pub class_ids: Vec<u64>,
    pub shallow_sizes: Vec<u32>,
    pub gc_root_addrs: Vec<u64>,
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
        let mut tmp_class_ids: Vec<u64> = Vec::new();
        let mut tmp_shallow: Vec<u32> = Vec::new();
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
                    class_map
                        .entry(class_addr)
                        .or_insert_with(ClassInfo::default)
                        .name_id = name_id;
                }
                tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                    scan_heap_segment(
                        &mut r,
                        id_size,
                        length,
                        &mut class_map,
                        &mut tmp_addrs,
                        &mut tmp_class_ids,
                        &mut tmp_shallow,
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

        // Fix up shallow sizes where class wasn't yet seen at scan time
        for (i, &cid) in tmp_class_ids.iter().enumerate() {
            if tmp_shallow[i] == 0 {
                if let Some(ci) = class_map.get(&cid) {
                    tmp_shallow[i] = ci.instance_size;
                }
            }
        }

        // Sort by address and deduplicate (same address may appear under multiple roots)
        let n = tmp_addrs.len();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_unstable_by_key(|&i| tmp_addrs[i]);

        let mut id_map = IdMap::with_capacity(n);
        let mut class_ids: Vec<u64> = Vec::with_capacity(n);
        let mut shallow_sizes: Vec<u32> = Vec::with_capacity(n);
        let mut prev_addr = u64::MAX;
        for &i in &order {
            let a = tmp_addrs[i];
            if a != prev_addr {
                id_map.push(a);
                class_ids.push(tmp_class_ids[i]);
                shallow_sizes.push(tmp_shallow[i]);
                prev_addr = a;
            }
        }
        id_map.sort_and_dedup(); // already sorted, just marks it done

        Ok(Pass1 {
            strings,
            class_map,
            class_serial_to_addr,
            id_map,
            class_ids,
            shallow_sizes,
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
    tmp_class_ids: &mut Vec<u64>,
    tmp_shallow: &mut Vec<u32>,
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
                tmp_class_ids.push(class_addr); // class-of-class resolved later in pass2
                tmp_shallow.push(0);            // pass2 recalculates shallow size for class objects
            }
            heap::INSTANCE_DUMP => {
                let addr = r.id()?;
                r.skip(4)?; // stack_trace_serial(u4)
                let class_id = r.id()?;
                let data_len = r.u4()? as u64;
                r.skip(data_len)?;
                let shallow = class_map.get(&class_id).map(|c| c.instance_size).unwrap_or(0);
                tmp_addrs.push(addr);
                tmp_class_ids.push(class_id);
                tmp_shallow.push(shallow);
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
                tmp_class_ids.push(elem_class_id);
                tmp_shallow.push(shallow);
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
                tmp_class_ids.push(elem_type_code as u64);
                tmp_shallow.push(shallow);
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
) -> io::Result<(u64, u64)> { // (class_addr, consumed)
    let ids = id_size as u64;
    let mut consumed: u64 = 0;

    let class_addr = r.id()?; consumed += ids;
    r.skip(4)?; consumed += 4; // stack_trace_serial
    let super_id = r.id()?; consumed += ids;
    let loader_id = r.id()?; consumed += ids;
    r.skip(ids)?; consumed += ids; // signers_id
    r.skip(ids)?; consumed += ids; // protection_domain_id
    r.skip(ids)?; consumed += ids; // reserved1
    r.skip(ids)?; consumed += ids; // reserved2
    let instance_size = r.u4()?; consumed += 4;

    // Constant pool entries
    let cp_count = r.u2()? as u64; consumed += 2;
    for _ in 0..cp_count {
        r.skip(2)?; consumed += 2; // cp index (u2)
        let cp_type = r.u1()?; consumed += 1;
        let vs = value_size(cp_type, id_size);
        r.skip(vs)?; consumed += vs;
    }

    // Static fields
    let static_count = r.u2()? as u64; consumed += 2;
    let mut static_obj_count = 0u32;
    let mut static_prim_bytes = 0u32;
    for _ in 0..static_count {
        r.skip(ids)?; consumed += ids; // name_id
        let field_type = r.u1()?; consumed += 1;
        let vs = value_size(field_type, id_size);
        r.skip(vs)?; consumed += vs;
        if field_type == 2 { static_obj_count += 1; }
        else { static_prim_bytes += vs as u32; }
    }

    // Instance fields
    let field_count = r.u2()? as u64; consumed += 2;
    let mut fields = Vec::with_capacity(field_count as usize);
    for _ in 0..field_count {
        let name_id = r.id()?; consumed += ids;
        let type_code = r.u1()?; consumed += 1;
        let htype = HprofType::from_code(type_code).unwrap_or(HprofType::Int);
        fields.push((name_id, htype));
    }

    let entry = class_map.entry(class_addr).or_insert_with(ClassInfo::default);
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
    const EXPECTED_INSTANCES:   u64 = 2_698_510;
    const EXPECTED_OBJ_ARRAYS:  u64 =   504_353;
    const EXPECTED_PRIM_ARRAYS: u64 =    27_379;
    const EXPECTED_CLASSES:     u64 =     2_646;
    const EXPECTED_GC_ROOTS:    u64 = 1_454 + 135 + 101; // 1,690 (JAVA_FRAME/JNI_LOCAL/etc -> thread_local_pairs)

    #[test]
    fn record_counts_match_diagnose() {
        if !std::path::Path::new(DUMP).exists() { return; }
        let p = Pass1::run(DUMP).unwrap();
        assert_eq!(p.instance_count,   EXPECTED_INSTANCES,   "instances");
        assert_eq!(p.obj_array_count,  EXPECTED_OBJ_ARRAYS,  "obj arrays");
        assert_eq!(p.prim_array_count, EXPECTED_PRIM_ARRAYS, "prim arrays");
        assert_eq!(p.class_dump_count, EXPECTED_CLASSES,     "classes");
        assert_eq!(p.gc_root_addrs.len() as u64, EXPECTED_GC_ROOTS,
            "gc roots (got {})", p.gc_root_addrs.len());
    }

    #[test]
    fn total_object_count() {
        if !std::path::Path::new(DUMP).exists() { return; }
        let p = Pass1::run(DUMP).unwrap();
        // id_map now includes class objects (from CLASS_DUMP records) in addition to instances/arrays
        let expected = EXPECTED_INSTANCES + EXPECTED_OBJ_ARRAYS + EXPECTED_PRIM_ARRAYS + p.class_dump_count;
        assert_eq!(p.id_map.len() as u64, expected,
            "total objects (got {})", p.id_map.len());
    }

    #[test]
    fn parallel_arrays_consistent() {
        if !std::path::Path::new(DUMP).exists() { return; }
        let p = Pass1::run(DUMP).unwrap();
        assert_eq!(p.id_map.len(), p.class_ids.len(), "class_ids len");
        assert_eq!(p.id_map.len(), p.shallow_sizes.len(), "shallow_sizes len");
    }

    #[test]
    fn header_fields() {
        if !std::path::Path::new(DUMP).exists() { return; }
        let p = Pass1::run(DUMP).unwrap();
        assert_eq!(p.id_size, 8);
        assert!(p.format.starts_with("JAVA PROFILE"));
        assert!(p.has_sticky_class_roots);
        assert!(!p.strings.is_empty());
    }

    #[test]
    fn class_name_resolvable() {
        if !std::path::Path::new(DUMP).exists() { return; }
        let p = Pass1::run(DUMP).unwrap();
        // Every class in class_map should have a name_id, and that name_id should be in strings
        let mut resolved = 0usize;
        for ci in p.class_map.values() {
            if p.strings.contains_key(&ci.name_id) { resolved += 1; }
        }
        // At least 90% of classes should have resolvable names
        let pct = resolved * 100 / p.class_map.len();
        assert!(pct >= 90, "only {pct}% of classes have resolvable names");
    }

    #[test]
    fn debug_root_coverage() {
        if !std::path::Path::new(DUMP).exists() { return; }
        let p = Pass1::run(DUMP).unwrap();
        let mut in_idmap = 0usize;
        let mut not_in_idmap = 0usize;
        let mut by_type: std::collections::HashMap<u8, (usize, usize)> = std::collections::HashMap::new();
        for (i, &addr) in p.gc_root_addrs.iter().enumerate() {
            let t = p.gc_root_types[i];
            let entry = by_type.entry(t).or_insert((0usize, 0usize));
            if p.id_map.index_of(addr).is_some() {
                in_idmap += 1;
                entry.0 += 1;
            } else {
                not_in_idmap += 1;
                entry.1 += 1;
            }
        }
        eprintln!("in_idmap={} not_in_idmap={}", in_idmap, not_in_idmap);
        let mut types: Vec<_> = by_type.iter().collect();
        types.sort_by_key(|(t, _)| **t);
        for (t, (found, missing)) in &types {
            eprintln!("  type=0x{:02x} found={} missing={}", t, found, missing);
        }
    }

}

    #[test]
    fn debug_gc_root_types() {
        let dump = "/home/i560383/test-heapdumps/dump_0_fj-kmeans.hprof";
        if !std::path::Path::new(dump).exists() { return; }
        let p = Pass1::run(dump).unwrap();
        let mut type_counts: std::collections::HashMap<u8, u32> = std::collections::HashMap::new();
        for &t in &p.gc_root_types {
            *type_counts.entry(t).or_insert(0) += 1;
        }
        eprintln!("GC root types:");
        for (t, cnt) in &type_counts {
            let name = match *t {
                0xff => "ROOT_UNKNOWN",
                0x01 => "ROOT_JNI_GLOBAL",
                0x02 => "ROOT_JNI_LOCAL",
                0x03 => "ROOT_JAVA_FRAME",
                0x04 => "ROOT_NATIVE_STACK",
                0x05 => "ROOT_STICKY_CLASS",
                0x06 => "ROOT_THREAD_BLOCK",
                0x07 => "ROOT_MONITOR_USED",
                0x08 => "ROOT_THREAD_OBJ",
                _ => "UNKNOWN",
            };
            eprintln!("  {} (0x{:02x}): {}", name, t, cnt);
        }
        eprintln!("Total gc_root_addrs: {}", p.gc_root_addrs.len());
    }

    #[test]
    fn debug_java_frame_root_classes() {
        let dump = "/home/i560383/test-heapdumps/dump_0_fj-kmeans.hprof";
        if !std::path::Path::new(dump).exists() { return; }
        let p = Pass1::run(dump).unwrap();
        let mut class_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for (i, &t) in p.gc_root_types.iter().enumerate() {
            if t == 0x03 { // JAVA_FRAME
                let addr = p.gc_root_addrs[i];
                let class_name = if let Some(idx) = p.id_map.index_of(addr) {
                    let cid = p.class_ids[idx];
                    if let Some(ci) = p.class_map.get(&cid) {
                        p.strings.get(&ci.name_id).cloned().unwrap_or_else(|| format!("@{cid:#x}"))
                    } else { format!("no_class@{cid:#x}") }
                } else { format!("not_in_idmap@{addr:#x}") };
                *class_counts.entry(class_name).or_insert(0) += 1;
            }
        }
        eprintln!("JAVA_FRAME root classes (top 15): total_entries={}", class_counts.len());
        let java_frame_count = p.gc_root_types.iter().filter(|&&t| t == 0x03).count();
        eprintln!("JAVA_FRAME root count: {}", java_frame_count);
        let mut sorted: Vec<(u32, &String)> = class_counts.iter().map(|(k,&v)| (v,k)).collect();
        sorted.sort_unstable_by(|a,b| b.0.cmp(&a.0));
        for (cnt, name) in sorted.iter().take(30) {
            eprintln!("  {:>8} {}", cnt, name);
        }
    }
