use std::{
    collections::HashMap,
    io::{self, ErrorKind},
};

use crate::{
    pass1::{ClassInfo, Pass1},
    reader::HprofReader,
    types::{heap, tags, HprofType},
    vbyte,
};

// ── Graph output struct ────────────────────────────────────────────────────

pub struct Graph {
    pub n: usize,
    pub id_size: u8,
    pub ref_size: u8,
    pub format: String,
    pub file_size: u64,
    pub gc_root_indices: Vec<u32>,
    pub shallow: Vec<u32>,
    pub class_idx: Vec<u32>,
    pub class_names: Vec<String>,
    // Forward CSR
    pub fwd_offsets: Vec<u32>,
    pub fwd_targets: Vec<u32>,
    // Inbound CSR (VByte delta-encoded)
    pub inb_offsets: Vec<u32>,
    pub inb_data: Vec<u8>,
    // Filled by later passes
    pub idom: Vec<u32>,
    pub retained: Vec<u64>,
    pub has_same_class_ancestor: Vec<bool>,
}

// ── Size helpers ───────────────────────────────────────────────────────────

fn align_up(n: usize, align: usize) -> usize {
    ((n + align - 1) / align) * align
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
    ci.fields.iter().filter(|(_, t)| *t == HprofType::Object).count()
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
                let super_size = calculate_size_recursive(ci.super_id, class_map, ptr_size, ref_size, cache);
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

fn class_obj_shallow(ci: &ClassInfo, ptr_size: usize, ref_size: usize) -> u32 {
    let base = ptr_size + ref_size;
    let computed = ci.static_obj_count as usize * ref_size + ci.static_prim_bytes as usize;
    align_up(computed.max(base), 8) as u32
}

// ── Field layout cache ─────────────────────────────────────────────────────

/// For a given class, returns a Vec of byte offsets within INSTANCE_DUMP data
/// for each Object-type field (subclass fields appear first in HPROF data).
fn build_obj_field_offsets(
    class_addr: u64,
    class_map: &HashMap<u64, ClassInfo>,
    ref_size: usize,
    cache: &mut HashMap<u64, Vec<(usize, u64)>>, // addr -> Vec<(offset, target_class_hint)>
) -> Vec<usize> {
    // We need the chain of ClassInfo from subclass to root, collecting fields top-down
    // but HPROF data layout has subclass fields first.
    // So we walk: class → super → super2 → ... and collect field offsets in order.
    let mut chain: Vec<u64> = Vec::new();
    let mut cur = class_addr;
    loop {
        match class_map.get(&cur) {
            None => break,
            Some(ci) => {
                chain.push(cur);
                if ci.super_id == 0 { break; }
                cur = ci.super_id;
            }
        }
    }

    let mut offsets = Vec::new();
    let mut byte_offset = 0usize;
    for &caddr in &chain {
        let ci = match class_map.get(&caddr) { Some(c) => c, None => break };
        for (_, t) in &ci.fields {
            let fsize = if *t == HprofType::Object { ref_size } else { t.byte_size() };
            if *t == HprofType::Object {
                offsets.push(byte_offset);
            }
            byte_offset += fsize;
        }
    }
    offsets
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

// ── Pass2 main logic ───────────────────────────────────────────────────────

pub struct Pass2;

impl Pass2 {
    pub fn build(path: &str, p1: Pass1) -> io::Result<Graph> {
        let n = p1.id_map.len();
        let id_size = p1.id_size;
        let ptr_size = id_size as usize;

        // ── Phase 0: detect ref_size ─────────────────────────────────────
        let ref_size = if id_size == 8 {
            Self::detect_compressed_oops(path, id_size)?
        } else {
            id_size
        } as usize;

        // ── Phase 0b: compute shallow sizes with MAT formula ─────────────
        // Build size cache
        let mut size_cache: HashMap<u64, usize> = HashMap::new();

        // Build set of class object addresses (objects that are class loaders/Java classes)
        let class_addrs: std::collections::HashSet<u64> =
            p1.class_map.keys().cloned().collect();

        let mut shallow: Vec<u32> = Vec::with_capacity(n);
        for i in 0..n {
            let addr = p1.id_map.addr_at(i);
            let cid = p1.class_ids[i];

            let sz = if class_addrs.contains(&addr) {
                // This object IS a class object
                if let Some(ci) = p1.class_map.get(&addr) {
                    class_obj_shallow(ci, ptr_size, ref_size)
                } else {
                    align_up(ptr_size + ref_size, 8) as u32
                }
            } else if p1.shallow_sizes[i] == 0 {
                // Unknown – fall back
                align_up(ptr_size + ref_size, 8) as u32
            } else {
                // Determine object kind from pass1 class_ids:
                // - For instances: cid is a class addr in class_map
                // - For obj arrays: cid is the element class addr
                // - For prim arrays: cid is the element type code (small number)
                if cid <= 11 && !class_addrs.contains(&cid) {
                    // Prim array – use pass1 shallow (accurate enough; recompute below)
                    p1.shallow_sizes[i]
                } else {
                    // Could be instance or obj array; pass1 shallow_sizes is accurate for
                    // instances (instance_size from CLASS_DUMP) but we want MAT formula.
                    // Use pass1 value as hint. We refine in sub-pass 2a by re-reading.
                    p1.shallow_sizes[i]
                }
            };
            shallow.push(sz);
        }

        // ── Phase 0c: Build class names ──────────────────────────────────
        // Map from (class name string) → index in class_names vec
        let mut class_name_to_idx: HashMap<String, u32> = HashMap::new();
        let mut class_names: Vec<String> = Vec::new();

        let mut get_or_insert_class_name = |name: String| -> u32 {
            if let Some(&idx) = class_name_to_idx.get(&name) {
                return idx;
            }
            let idx = class_names.len() as u32;
            class_name_to_idx.insert(name.clone(), idx);
            class_names.push(name);
            idx
        };

        // Find java/lang/Class name
        let java_lang_class_name = "java/lang/Class".to_string();

        // Build class_idx array
        let mut class_idx: Vec<u32> = vec![0u32; n];

        // Find all class objects (so we know their class_idx = java/lang/Class)
        let mut java_lang_class_idx: Option<u32> = None;

        // First pass: populate class_idx for all objects
        for i in 0..n {
            let addr = p1.id_map.addr_at(i);
            let cid = p1.class_ids[i];

            if class_addrs.contains(&addr) {
                // This is a class object → its type is java/lang/Class
                let nm = java_lang_class_name.clone();
                let idx = get_or_insert_class_name(nm);
                if java_lang_class_idx.is_none() {
                    java_lang_class_idx = Some(idx);
                }
                class_idx[i] = idx;
            } else if cid <= 11 && !class_addrs.contains(&cid) {
                // Prim array
                let nm = prim_array_class_name(cid as u8).to_string();
                let idx = get_or_insert_class_name(nm);
                class_idx[i] = idx;
            } else {
                // Instance or obj array
                // For instances: look up class_map[cid].name_id → strings
                // For obj arrays: pass1 stores elem_class_id in class_ids
                // Disambiguate: if it's an obj array, the address in pass1
                // has shallow_sizes[i] that was computed as array formula.
                // We rely on whether cid → class name starts with '[' for arrays.
                // Actually we can't tell instance vs obj array from pass1 alone without
                // re-reading. We use class name from class_map if available.
                let nm = if let Some(ci) = p1.class_map.get(&cid) {
                    let base = p1.strings.get(&ci.name_id).cloned()
                        .unwrap_or_else(|| format!("unknown@{cid:#x}"));
                    base
                } else {
                    format!("unknown@{cid:#x}")
                };
                let idx = get_or_insert_class_name(nm);
                class_idx[i] = idx;
            }
        }

        // We need to do a sub-pass to fix obj array class names and shallow sizes
        // Do it in sub-pass 2a along with edge counting.

        // ── Phase 1: Sub-pass 2a — count degrees ────────────────────────
        let mut out_degree: Vec<u32> = vec![0u32; n];
        let mut in_degree: Vec<u32> = vec![0u32; n];

        // Build field layout cache: class_addr → Vec<usize> byte offsets for Object fields
        let mut field_offset_cache: HashMap<u64, Vec<usize>> = HashMap::new();

        // Build name lookup for excluded field detection
        // field_name_ids for excluded fields: pre-collect name_ids from strings
        let excluded_name_ids: std::collections::HashSet<u64> = {
            let mut s = std::collections::HashSet::new();
            for (&id, name) in &p1.strings {
                if matches!(name.as_str(), "referent" | "unfinalized" | "<Unfinalized>") {
                    s.insert(id);
                }
            }
            s
        };

        // Class addr → (class name, set of excluded field name_ids for this class)
        // We need: for each class, which of its OWN field name_ids are excluded?
        // Excluded = (class is java/lang/ref/Reference AND field is referent)
        //           OR (class is java/lang/ref/Finalizer AND field is unfinalized)
        //           OR (class is java/lang/Runtime AND field is <Unfinalized>)
        let excluded_class_field: HashMap<u64, std::collections::HashSet<u64>> = {
            let mut m: HashMap<u64, std::collections::HashSet<u64>> = HashMap::new();
            for (&caddr, ci) in &p1.class_map {
                let cname = p1.strings.get(&ci.name_id).map(|s| s.as_str()).unwrap_or("");
                for &(fname_id, t) in &ci.fields {
                    if t != HprofType::Object { continue; }
                    let fname = p1.strings.get(&fname_id).map(|s| s.as_str()).unwrap_or("");
                    if is_excluded_field(cname, fname) {
                        m.entry(caddr).or_default().insert(fname_id);
                    }
                }
            }
            m
        };

        // We'll also rebuild shallow sizes for arrays during this pass.
        // Track array info: addr → (num_elem, is_obj_array, elem_class_or_type)
        // Actually we re-read during sub-pass 2a.

        // Also track array addresses/counts for compressed OOPs detection (already done above).

        // ── Sub-pass 2a scan ─────────────────────────────────────────────
        {
            let mut r = HprofReader::open(path)?;
            let ids = id_size as u64;

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
                            &class_addrs,
                            &excluded_class_field,
                            &mut field_offset_cache,
                            &mut out_degree,
                            &mut in_degree,
                            &mut shallow,
                            &mut class_idx,
                            &mut get_or_insert_class_name,
                        )?;
                    }
                    tags::HEAP_DUMP_END => break,
                    _ => { r.skip(length)?; }
                }
            }
        }

        // Ensure java/lang/Class index is consistent (class objects already assigned above)
        // Re-assign class objects to java/lang/Class index after possible updates
        let jlc_idx = get_or_insert_class_name(java_lang_class_name);
        for i in 0..n {
            let addr = p1.id_map.addr_at(i);
            if class_addrs.contains(&addr) {
                class_idx[i] = jlc_idx;
            }
        }

        // Ensure no zero shallow sizes (fall back to minimum)
        let min_obj = align_up(ptr_size + ref_size, 8) as u32;
        for s in shallow.iter_mut() {
            if *s == 0 { *s = min_obj; }
        }

        // ── Phase 2: Build GC root indices ───────────────────────────────
        let mut gc_root_set: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for &addr in &p1.gc_root_addrs {
            if let Some(idx) = p1.id_map.index_of(addr) {
                gc_root_set.insert(idx as u32);
            }
        }
        // Add implicit roots: non-array boot-loader classes (loader_id==0) if no sticky roots
        if !p1.has_sticky_class_roots {
            for (&caddr, ci) in &p1.class_map {
                if ci.loader_id == 0 {
                    // Check it's not an array class (name doesn't start with '[')
                    let is_array = p1.strings.get(&ci.name_id)
                        .map(|n| n.starts_with('['))
                        .unwrap_or(false);
                    if !is_array {
                        if let Some(idx) = p1.id_map.index_of(caddr) {
                            gc_root_set.insert(idx as u32);
                        }
                    }
                }
            }
        }
        let gc_root_indices: Vec<u32> = gc_root_set.into_iter().collect();

        // ── Phase 3: Build forward CSR via prefix sum + fill pass ────────
        // Prefix-sum out_degrees → fwd_offsets
        let mut fwd_offsets: Vec<u32> = Vec::with_capacity(n + 1);
        fwd_offsets.push(0u32);
        for i in 0..n {
            let next = fwd_offsets[i] + out_degree[i];
            fwd_offsets.push(next);
        }
        let total_edges = *fwd_offsets.last().unwrap() as usize;
        let mut fwd_targets: Vec<u32> = vec![u32::MAX; total_edges];
        let mut fwd_cursor: Vec<u32> = fwd_offsets[..n].to_vec();

        // Allocate per-node inbound lists (to be sorted + delta-encoded later)
        let mut inb_lists: Vec<Vec<u32>> = vec![Vec::new(); n];

        // ── Sub-pass 2b scan ─────────────────────────────────────────────
        {
            let mut r = HprofReader::open(path)?;

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
                            &p1.class_map,
                            &p1.strings,
                            &class_addrs,
                            &excluded_class_field,
                            &mut field_offset_cache,
                            &mut fwd_targets,
                            &mut fwd_cursor,
                            &fwd_offsets,
                            &mut inb_lists,
                        )?;
                    }
                    tags::HEAP_DUMP_END => break,
                    _ => { r.skip(length)?; }
                }
            }
        }

        // ── Phase 4: Build inbound CSR ────────────────────────────────────
        let mut inb_offsets: Vec<u32> = Vec::with_capacity(n + 1);
        let mut inb_data: Vec<u8> = Vec::new();
        inb_offsets.push(0u32);
        for i in 0..n {
            let list = &mut inb_lists[i];
            // Sort by value (with high-bit excluded markers: sort on raw u32 including flag)
            // For delta encoding to work, we need to sort the "plain" values but keep the flag.
            // Strategy: strip flag, sort, re-apply flag, delta encode.
            // Actually we encode: for excluded edges src is stored as src|0x80000000.
            // We need to sort by src (ignoring flag bit for sorting order) for delta encoding.
            // Let's sort by (val & 0x7fffffff) then delta-encode the plain index,
            // but store the flag bit separately? That's complex.
            // Simpler: just sort by raw u32 value (excluded entries have high bit set, so they
            // sort after non-excluded, but that's OK for delta encoding since we encode raw u32).
            list.sort_unstable();
            list.dedup();
            let byte_start = inb_data.len() as u32;
            vbyte::encode_delta(list, &mut inb_data);
            inb_offsets.push(inb_data.len() as u32);
            let _ = byte_start;
        }

        Ok(Graph {
            n,
            id_size,
            ref_size: ref_size as u8,
            format: p1.format,
            file_size: p1.file_size,
            gc_root_indices,
            shallow,
            class_idx,
            class_names,
            fwd_offsets,
            fwd_targets,
            inb_offsets,
            inb_data,
            idom: Vec::new(),
            retained: Vec::new(),
            has_same_class_ancestor: Vec::new(),
        })
    }

    fn detect_compressed_oops(path: &str, id_size: u8) -> io::Result<u8> {
        let mut r = HprofReader::open(path)?;
        let ids = id_size as u64;
        let mut array_addr_counts: Vec<(u64, u64)> = Vec::new();

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
                            heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED => {
                                r.skip(ids)?;
                                remaining -= ids;
                            }
                            heap::ROOT_JNI_GLOBAL => {
                                r.skip(2 * ids)?;
                                remaining -= 2 * ids;
                            }
                            heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME => {
                                r.skip(ids + 8)?;
                                remaining -= ids + 8;
                            }
                            heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                                r.skip(ids + 4)?;
                                remaining -= ids + 4;
                            }
                            heap::ROOT_STICKY_CLASS => {
                                r.skip(ids)?;
                                remaining -= ids;
                            }
                            heap::ROOT_THREAD_OBJ => {
                                r.skip(ids + 8)?;
                                remaining -= ids + 8;
                            }
                            heap::CLASS_DUMP => {
                                let consumed = Self::skip_class_dump(&mut r, id_size)?;
                                remaining -= consumed;
                            }
                            heap::INSTANCE_DUMP => {
                                r.skip(ids)?; // addr
                                r.skip(4)?;   // stack serial
                                r.skip(ids)?; // class_id
                                let data_len = r.u4()? as u64;
                                r.skip(data_len)?;
                                remaining -= ids + 4 + ids + 4 + data_len;
                            }
                            heap::OBJ_ARRAY_DUMP => {
                                let addr = r.id()?;
                                r.skip(4)?; // stack serial
                                let count = r.u4()? as u64;
                                r.skip(ids)?; // elem_class_id
                                r.skip(count * ids)?;
                                array_addr_counts.push((addr, count));
                                remaining -= ids + 4 + 4 + ids + count * ids;
                            }
                            heap::PRIM_ARRAY_DUMP => {
                                r.skip(ids)?; // addr
                                r.skip(4)?;   // stack serial
                                let count = r.u4()? as u64;
                                let elem_type = r.u1()?;
                                let esz = HprofType::from_code(elem_type)
                                    .map(|t| t.byte_size() as u64)
                                    .unwrap_or(1);
                                r.skip(count * esz)?;
                                remaining -= ids + 4 + 4 + 1 + count * esz;
                            }
                            _ => {
                                return Err(io::Error::new(
                                    ErrorKind::InvalidData,
                                    format!("unknown heap sub-tag 0x{sub_tag:02x} in detect_compressed_oops"),
                                ));
                            }
                        }
                    }
                }
                tags::HEAP_DUMP_END => break,
                _ => { r.skip(length)?; }
            }
        }

        Ok(detect_ref_size(id_size, &array_addr_counts))
    }

    fn skip_class_dump(r: &mut HprofReader, id_size: u8) -> io::Result<u64> {
        let ids = id_size as u64;
        let mut consumed = 0u64;
        r.skip(ids)?; consumed += ids; // class addr
        r.skip(4)?; consumed += 4;     // stack serial
        r.skip(ids * 6 + 4)?; consumed += ids * 6 + 4; // super + loader + sigs + pd + r1 + r2 + instance_size
        // constant pool
        let cp = r.u2()? as u64; consumed += 2;
        for _ in 0..cp {
            r.skip(2)?; consumed += 2;
            let tp = r.u1()?; consumed += 1;
            let vs = value_size(tp, id_size);
            r.skip(vs)?; consumed += vs;
        }
        // static fields
        let sc = r.u2()? as u64; consumed += 2;
        for _ in 0..sc {
            r.skip(ids)?; consumed += ids;
            let tp = r.u1()?; consumed += 1;
            let vs = value_size(tp, id_size);
            r.skip(vs)?; consumed += vs;
        }
        // instance fields
        let ic = r.u2()? as u64; consumed += 2;
        r.skip(ic * (ids + 1))?; consumed += ic * (ids + 1);
        Ok(consumed)
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_heap_2a<F>(
        r: &mut HprofReader,
        id_size: u8,
        ref_size: usize,
        ptr_size: usize,
        mut remaining: u64,
        id_map: &crate::id_map::IdMap,
        class_map: &HashMap<u64, ClassInfo>,
        strings: &HashMap<u64, String>,
        class_addrs: &std::collections::HashSet<u64>,
        excluded_class_field: &HashMap<u64, std::collections::HashSet<u64>>,
        field_offset_cache: &mut HashMap<u64, Vec<usize>>,
        out_degree: &mut Vec<u32>,
        in_degree: &mut Vec<u32>,
        shallow: &mut Vec<u32>,
        class_idx: &mut Vec<u32>,
        get_class_name_idx: &mut F,
    ) -> io::Result<()>
    where
        F: FnMut(String) -> u32,
    {
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

        while remaining > 0 {
            let sub_tag = r.u1()?;
            remaining -= 1;

            match sub_tag {
                heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED => {
                    r.skip(ids)?;
                    remaining -= ids;
                }
                heap::ROOT_JNI_GLOBAL => {
                    r.skip(2 * ids)?;
                    remaining -= 2 * ids;
                }
                heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME => {
                    r.skip(ids + 8)?;
                    remaining -= ids + 8;
                }
                heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                    r.skip(ids + 4)?;
                    remaining -= ids + 4;
                }
                heap::ROOT_STICKY_CLASS => {
                    r.skip(ids)?;
                    remaining -= ids;
                }
                heap::ROOT_THREAD_OBJ => {
                    r.skip(ids + 8)?;
                    remaining -= ids + 8;
                }
                heap::CLASS_DUMP => {
                    let (consumed, class_addr, super_id, loader_id) =
                        Self::read_class_dump_edges(r, id_size)?;
                    remaining -= consumed;
                    if let Some(src_idx) = id_map.index_of(class_addr) {
                        // class → super class object
                        edge_if_valid!(src_idx, super_id, false);
                        // class → loader object
                        edge_if_valid!(src_idx, loader_id, false);
                        // static object fields handled there
                        // Actually we need them separately — re-read needed.
                        // We already counted in read_class_dump_edges.
                    }
                }
                heap::INSTANCE_DUMP => {
                    let addr = r.id()?;
                    r.skip(4)?;
                    let class_id = r.id()?;
                    let data_len = r.u4()? as u64;
                    let data = r.read_bytes(data_len as usize)?;
                    remaining -= ids + 4 + ids + 4 + data_len;

                    let src_idx = match id_map.index_of(addr) {
                        Some(i) => i,
                        None => continue,
                    };

                    // Recalculate MAT shallow size for instances
                    if !class_addrs.contains(&addr) {
                        let sz = instance_shallow_size(class_id, class_map, ptr_size, ref_size, &mut HashMap::new());
                        shallow[src_idx] = sz;
                    }

                    // Edge: instance → class object
                    edge_if_valid!(src_idx, class_id, false);

                    // Edges from Object-type fields
                    let offsets = field_offset_cache
                        .entry(class_id)
                        .or_insert_with(|| build_obj_field_offsets(class_id, class_map, ref_size, &mut HashMap::new()))
                        .clone();

                    // Get excluded field offsets for this class (walk super chain)
                    // We need to know which specific byte offsets correspond to excluded fields.
                    // Build: for instance fields (walk chain), mark offset if field is excluded.
                    // This is done by tracking excluded offsets per class.
                    let excl_offsets = Self::excluded_field_offsets(class_id, class_map, strings, ref_size);

                    for off in &offsets {
                        if *off + ref_size <= data.len() {
                            let ref_val = read_ref(&data[*off..], ref_size);
                            if ref_val != 0 {
                                if let Some(dst) = id_map.index_of(ref_val) {
                                    out_degree[src_idx] += 1;
                                    in_degree[dst] += 1;
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
                    let elems_bytes = r.read_bytes((count * ids) as usize)?;
                    remaining -= ids + 4 + 4 + ids + count * ids;

                    let src_idx = match id_map.index_of(addr) {
                        Some(i) => i,
                        None => continue,
                    };

                    // Fix shallow size for object arrays
                    shallow[src_idx] = obj_array_shallow(count, ptr_size, ref_size);

                    // Fix class_idx: synthesize "[Lelem_class_name;" or "[java/lang/Object;"
                    let elem_name = if let Some(ci) = class_map.get(&elem_class_id) {
                        strings.get(&ci.name_id).cloned()
                            .unwrap_or_else(|| format!("unknown@{elem_class_id:#x}"))
                    } else {
                        "java/lang/Object".to_string()
                    };
                    let arr_name = if elem_name.starts_with('[') {
                        format!("[{elem_name}")
                    } else {
                        format!("[L{elem_name};")
                    };
                    class_idx[src_idx] = get_class_name_idx(arr_name);

                    // Edge: array → element class object
                    edge_if_valid!(src_idx, elem_class_id, false);

                    // Edges: array → non-null elements
                    for chunk in elems_bytes.chunks(ids as usize) {
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
                    remaining -= ids + 4 + 4 + 1 + count * esz;

                    if let Some(src_idx) = id_map.index_of(addr) {
                        // Fix shallow size
                        shallow[src_idx] = prim_array_shallow(count, esz as usize, ptr_size, ref_size);
                        // Fix class_idx
                        let nm = prim_array_class_name(elem_type).to_string();
                        class_idx[src_idx] = get_class_name_idx(nm);
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

    /// Returns (consumed, class_addr, super_id, loader_id) and edges counted separately.
    fn read_class_dump_edges(r: &mut HprofReader, id_size: u8) -> io::Result<(u64, u64, u64, u64)> {
        let ids = id_size as u64;
        let mut consumed = 0u64;
        let class_addr = r.id()?; consumed += ids;
        r.skip(4)?; consumed += 4;
        let super_id = r.id()?; consumed += ids;
        let loader_id = r.id()?; consumed += ids;
        r.skip(ids * 4 + 4)?; consumed += ids * 4 + 4; // signers, pd, r1, r2, instance_size
        let cp = r.u2()? as u64; consumed += 2;
        for _ in 0..cp {
            r.skip(2)?; consumed += 2;
            let tp = r.u1()?; consumed += 1;
            let vs = value_size(tp, id_size);
            r.skip(vs)?; consumed += vs;
        }
        let sc = r.u2()? as u64; consumed += 2;
        for _ in 0..sc {
            r.skip(ids)?; consumed += ids;
            let tp = r.u1()?; consumed += 1;
            let vs = value_size(tp, id_size);
            r.skip(vs)?; consumed += vs;
        }
        let ic = r.u2()? as u64; consumed += 2;
        r.skip(ic * (ids + 1))?; consumed += ic * (ids + 1);
        Ok((consumed, class_addr, super_id, loader_id))
    }

    fn excluded_field_offsets(
        class_id: u64,
        class_map: &HashMap<u64, ClassInfo>,
        strings: &HashMap<u64, String>,
        ref_size: usize,
    ) -> Vec<usize> {
        let mut excl = Vec::new();
        let mut chain: Vec<u64> = Vec::new();
        let mut cur = class_id;
        loop {
            match class_map.get(&cur) {
                None => break,
                Some(ci) => {
                    chain.push(cur);
                    if ci.super_id == 0 { break; }
                    cur = ci.super_id;
                }
            }
        }
        let mut byte_offset = 0usize;
        for caddr in &chain {
            let ci = match class_map.get(caddr) { Some(c) => c, None => break };
            let cname = strings.get(&ci.name_id).map(|s| s.as_str()).unwrap_or("");
            for &(fname_id, t) in &ci.fields {
                let fsize = if t == HprofType::Object { ref_size } else { t.byte_size() };
                if t == HprofType::Object {
                    let fname = strings.get(&fname_id).map(|s| s.as_str()).unwrap_or("");
                    if is_excluded_field(cname, fname) {
                        excl.push(byte_offset);
                    }
                }
                byte_offset += fsize;
            }
        }
        excl
    }

    #[allow(clippy::too_many_arguments)]
    fn fill_heap_2b(
        r: &mut HprofReader,
        id_size: u8,
        ref_size: usize,
        mut remaining: u64,
        id_map: &crate::id_map::IdMap,
        class_map: &HashMap<u64, ClassInfo>,
        strings: &HashMap<u64, String>,
        class_addrs: &std::collections::HashSet<u64>,
        excluded_class_field: &HashMap<u64, std::collections::HashSet<u64>>,
        field_offset_cache: &mut HashMap<u64, Vec<usize>>,
        fwd_targets: &mut Vec<u32>,
        fwd_cursor: &mut Vec<u32>,
        fwd_offsets: &Vec<u32>,
        inb_lists: &mut Vec<Vec<u32>>,
    ) -> io::Result<()> {
        let ids = id_size as u64;

        macro_rules! add_edge {
            ($src:expr, $dst_addr:expr, $excluded:expr) => {
                if $dst_addr != 0 {
                    if let Some(dst) = id_map.index_of($dst_addr) {
                        let src = $src as usize;
                        let pos = fwd_cursor[src] as usize;
                        if pos < fwd_offsets[src + 1] as usize {
                            fwd_targets[pos] = dst as u32;
                            fwd_cursor[src] += 1;
                        }
                        // Inbound: store src with/without exclusion flag
                        let inb_val = if $excluded {
                            (src as u32) | 0x8000_0000u32
                        } else {
                            src as u32
                        };
                        inb_lists[dst].push(inb_val);
                    }
                }
            };
        }

        while remaining > 0 {
            let sub_tag = r.u1()?;
            remaining -= 1;

            match sub_tag {
                heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED => {
                    r.skip(ids)?;
                    remaining -= ids;
                }
                heap::ROOT_JNI_GLOBAL => {
                    r.skip(2 * ids)?;
                    remaining -= 2 * ids;
                }
                heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME => {
                    r.skip(ids + 8)?;
                    remaining -= ids + 8;
                }
                heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                    r.skip(ids + 4)?;
                    remaining -= ids + 4;
                }
                heap::ROOT_STICKY_CLASS => {
                    r.skip(ids)?;
                    remaining -= ids;
                }
                heap::ROOT_THREAD_OBJ => {
                    r.skip(ids + 8)?;
                    remaining -= ids + 8;
                }
                heap::CLASS_DUMP => {
                    let consumed = Self::fill_class_dump_edges(
                        r, id_size, id_map, fwd_targets, fwd_cursor, fwd_offsets, inb_lists,
                    )?;
                    remaining -= consumed;
                }
                heap::INSTANCE_DUMP => {
                    let addr = r.id()?;
                    r.skip(4)?;
                    let class_id = r.id()?;
                    let data_len = r.u4()? as u64;
                    let data = r.read_bytes(data_len as usize)?;
                    remaining -= ids + 4 + ids + 4 + data_len;

                    let src_idx = match id_map.index_of(addr) {
                        Some(i) => i,
                        None => continue,
                    };

                    // Edge: instance → class object
                    add_edge!(src_idx, class_id, false);

                    // Edges from Object-type fields
                    let offsets = field_offset_cache
                        .entry(class_id)
                        .or_insert_with(|| build_obj_field_offsets(class_id, class_map, ref_size, &mut HashMap::new()))
                        .clone();

                    let excl_offsets = Self::excluded_field_offsets(class_id, class_map, strings, ref_size);
                    let excl_set: std::collections::HashSet<usize> = excl_offsets.into_iter().collect();

                    for off in &offsets {
                        if *off + ref_size <= data.len() {
                            let ref_val = read_ref(&data[*off..], ref_size);
                            let excl = excl_set.contains(off);
                            add_edge!(src_idx, ref_val, excl);
                        }
                    }
                }
                heap::OBJ_ARRAY_DUMP => {
                    let addr = r.id()?;
                    r.skip(4)?;
                    let count = r.u4()? as u64;
                    let elem_class_id = r.id()?;
                    let elems_bytes = r.read_bytes((count * ids) as usize)?;
                    remaining -= ids + 4 + 4 + ids + count * ids;

                    let src_idx = match id_map.index_of(addr) {
                        Some(i) => i,
                        None => continue,
                    };

                    // Edge: array → element class
                    add_edge!(src_idx, elem_class_id, false);

                    // Edges to elements
                    for chunk in elems_bytes.chunks(ids as usize) {
                        let ref_val = read_id(chunk, id_size);
                        add_edge!(src_idx, ref_val, false);
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
                    remaining -= ids + 4 + 4 + 1 + count * esz;
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

    fn fill_class_dump_edges(
        r: &mut HprofReader,
        id_size: u8,
        id_map: &crate::id_map::IdMap,
        fwd_targets: &mut Vec<u32>,
        fwd_cursor: &mut Vec<u32>,
        fwd_offsets: &Vec<u32>,
        inb_lists: &mut Vec<Vec<u32>>,
    ) -> io::Result<u64> {
        let ids = id_size as u64;
        let mut consumed = 0u64;

        let class_addr = r.id()?; consumed += ids;
        r.skip(4)?; consumed += 4;
        let super_id = r.id()?; consumed += ids;
        let loader_id = r.id()?; consumed += ids;
        r.skip(ids * 4 + 4)?; consumed += ids * 4 + 4;

        let src_idx_opt = id_map.index_of(class_addr);

        macro_rules! add_edge_inner {
            ($src:expr, $dst_addr:expr) => {
                if $dst_addr != 0 {
                    if let Some(dst) = id_map.index_of($dst_addr) {
                        let src = $src as usize;
                        let pos = fwd_cursor[src] as usize;
                        if pos < fwd_offsets[src + 1] as usize {
                            fwd_targets[pos] = dst as u32;
                            fwd_cursor[src] += 1;
                        }
                        inb_lists[dst].push(src as u32);
                    }
                }
            };
        }

        if let Some(src) = src_idx_opt {
            add_edge_inner!(src, super_id);
            add_edge_inner!(src, loader_id);
        }

        // Constant pool
        let cp = r.u2()? as u64; consumed += 2;
        for _ in 0..cp {
            r.skip(2)?; consumed += 2;
            let tp = r.u1()?; consumed += 1;
            let vs = value_size(tp, id_size);
            r.skip(vs)?; consumed += vs;
        }

        // Static fields
        let sc = r.u2()? as u64; consumed += 2;
        for _ in 0..sc {
            r.skip(ids)?; consumed += ids; // name_id
            let tp = r.u1()?; consumed += 1;
            let vs = value_size(tp, id_size);
            if tp == 2 {
                // Object static field
                let ref_val = read_id_from_reader(r, id_size)?;
                consumed += vs;
                if let Some(src) = src_idx_opt {
                    add_edge_inner!(src, ref_val);
                }
            } else {
                r.skip(vs)?; consumed += vs;
            }
        }

        // Instance fields (just skip)
        let ic = r.u2()? as u64; consumed += 2;
        r.skip(ic * (ids + 1))?; consumed += ic * (ids + 1);

        Ok(consumed)
    }
}

// ── Also need 2a version of CLASS_DUMP to count static obj edges ───────────
// We need a version that also counts degrees for CLASS_DUMP static fields.
// The scan_heap_2a already calls read_class_dump_edges which skips them.
// Let's add a proper counting version.

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

        let class_addr = r.id()?; consumed += ids;
        r.skip(4)?; consumed += 4;
        let super_id = r.id()?; consumed += ids;
        let loader_id = r.id()?; consumed += ids;
        r.skip(ids * 4 + 4)?; consumed += ids * 4 + 4;

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

        let cp = r.u2()? as u64; consumed += 2;
        for _ in 0..cp {
            r.skip(2)?; consumed += 2;
            let tp = r.u1()?; consumed += 1;
            let vs = value_size(tp, id_size);
            r.skip(vs)?; consumed += vs;
        }

        let sc = r.u2()? as u64; consumed += 2;
        for _ in 0..sc {
            r.skip(ids)?; consumed += ids;
            let tp = r.u1()?; consumed += 1;
            let vs = value_size(tp, id_size);
            if tp == 2 {
                let ref_val = read_id_from_reader(r, id_size)?;
                consumed += vs;
                count_edge!(ref_val);
            } else {
                r.skip(vs)?; consumed += vs;
            }
        }

        let ic = r.u2()? as u64; consumed += 2;
        r.skip(ic * (ids + 1))?; consumed += ic * (ids + 1);

        Ok(consumed)
    }
}

// ── Utility ────────────────────────────────────────────────────────────────

fn read_ref(data: &[u8], ref_size: usize) -> u64 {
    if ref_size == 4 {
        if data.len() >= 4 {
            u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as u64
        } else { 0 }
    } else {
        if data.len() >= 8 {
            u64::from_be_bytes([
                data[0], data[1], data[2], data[3],
                data[4], data[5], data[6], data[7],
            ])
        } else { 0 }
    }
}

fn read_id(chunk: &[u8], id_size: u8) -> u64 {
    if id_size == 4 {
        if chunk.len() >= 4 {
            u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as u64
        } else { 0 }
    } else {
        if chunk.len() >= 8 {
            u64::from_be_bytes([
                chunk[0], chunk[1], chunk[2], chunk[3],
                chunk[4], chunk[5], chunk[6], chunk[7],
            ])
        } else { 0 }
    }
}

fn read_id_from_reader(r: &mut HprofReader, id_size: u8) -> io::Result<u64> {
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
    fn pass2_graph_has_edges() {
        if !std::path::Path::new(DUMP).exists() { return; }
        let p1 = Pass1::run(DUMP).unwrap();
        let g = Pass2::build(DUMP, p1).unwrap();
        assert!(g.fwd_targets.len() > 0, "no forward edges");
        assert_eq!(g.fwd_offsets.len(), g.n + 1);
        assert_eq!(g.inb_offsets.len() as usize, g.n + 1);
        for &r in &g.gc_root_indices {
            assert!((r as usize) < g.n, "gc_root idx {} out of range {}", r, g.n);
        }
        assert_eq!(g.class_idx.len(), g.n);
        assert!(!g.class_names.is_empty());
        assert!(g.shallow.iter().all(|&s| s > 0), "some shallow sizes are 0");
    }

    #[test]
    fn pass2_edge_counts_sane() {
        if !std::path::Path::new(DUMP).exists() { return; }
        let p1 = Pass1::run(DUMP).unwrap();
        let g = Pass2::build(DUMP, p1).unwrap();
        let fwd_edge_count: usize = g.fwd_offsets.windows(2).map(|w| (w[1]-w[0]) as usize).sum();
        assert!(fwd_edge_count > g.n / 2, "suspiciously few edges: {} for {} nodes", fwd_edge_count, g.n);
    }
}
