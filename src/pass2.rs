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
    pub source_name: String,
    pub gc_root_indices: Vec<u32>,
    pub shallow: Vec<u32>,
    pub class_idx: Vec<u32>,
    pub class_names: Vec<String>,
    pub class_obj_class_idx: Vec<u32>,  // per obj: which class it represents (u32::MAX if not class obj)
    // Forward CSR
    pub fwd_offsets: Vec<u32>,
    pub fwd_targets: Vec<u32>,
    // Inbound CSR (VByte delta-encoded)
    pub inb_offsets: Vec<u32>,
    pub inb_data: Vec<u8>,
    /// Number of GC roots added synthetically (system class roots, etc.)
    /// Reported GC roots = gc_root_indices.len() - synthetic_root_count
    pub synthetic_root_count: usize,
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
    for (&class_addr, _) in class_map {
        chain.clear();
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
        let mut plan: FieldPlan = Vec::new();
        let mut byte_offset = 0usize;
        for &caddr in &chain {
            let ci = match class_map.get(&caddr) { Some(c) => c, None => break };
            let cname = strings.get(&ci.name_id).map(|s| s.as_str()).unwrap_or("");
            for &(fname_id, t) in &ci.fields {
                let fsize = if t == HprofType::Object { id_size } else { t.byte_size() };
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

// ── Pass2 main logic ───────────────────────────────────────────────────────

pub struct Pass2;

impl Pass2 {
    pub fn build(path: &str, mut p1: Pass1) -> io::Result<Graph> {
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
        let class_addrs: std::collections::HashSet<u64> =
            p1.class_map.keys().cloned().collect();

        let mut shallow: Vec<u32> = Vec::with_capacity(n);
        for i in 0..n {
            let cid = p1.class_ids[i];
            let sz = match p1.kind[i] {
                3 => {
                    // Class object: shallow from static fields only, attributed to java.lang.Class.
                    match p1.class_map.get(&cid) {
                        Some(ci) => class_obj_shallow(ci, ptr_size, ref_size),
                        None => align_up(ptr_size + ref_size, 8) as u32,
                    }
                }
                1 => {
                    // Object array: cid is the array class id (elem count from pass1).
                    obj_array_shallow(p1.elem_count[i] as u64, ptr_size, ref_size)
                }
                2 => {
                    // Primitive array: cid is the element type code.
                    let elem_size = HprofType::from_code(cid as u8)
                        .map(|t| t.byte_size())
                        .unwrap_or(1);
                    prim_array_shallow(p1.elem_count[i] as u64, elem_size, ptr_size, ref_size)
                }
                _ => {
                    // Instance: MAT calculateSizeRecursive over the super chain.
                    if p1.class_map.contains_key(&cid) {
                        instance_shallow_size(cid, &p1.class_map, ptr_size, ref_size, &mut size_cache)
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

        let mut get_or_insert_class = |key: u64, name: &dyn Fn() -> String| -> u32 {
            if let Some(&idx) = class_key_to_idx.get(&key) {
                return idx;
            }
            let idx = class_names.len() as u32;
            class_key_to_idx.insert(key, idx);
            class_names.push(name());
            idx
        };

        // Build class_idx array
        let mut class_idx: Vec<u32> = vec![0u32; n];

        // First pass: populate class_idx for all objects (kind-driven, no heuristics)
        for i in 0..n {
            let cid = p1.class_ids[i];

            match p1.kind[i] {
                3 => {
                    // Class object → single java/lang/Class row (MAT parity)
                    class_idx[i] = get_or_insert_class(JLC_KEY, &|| "java/lang/Class".to_string());
                }
                2 => {
                    // Primitive array: cid is the element type code.
                    let tc = cid as u8;
                    class_idx[i] = get_or_insert_class(
                        PRIM_KEY_BASE | tc as u64,
                        &|| prim_array_class_name(tc).to_string(),
                    );
                }
                1 => {
                    // Object array: cid is the array-class address (loader-distinct).
                    class_idx[i] = get_or_insert_class(cid, &|| {
                        p1.class_map
                            .get(&cid)
                            .and_then(|ci| p1.strings.get(&ci.name_id).cloned())
                            .unwrap_or_else(|| "[Ljava/lang/Object;".to_string())
                    });
                }
                _ => {
                    // Instance: cid is the class-object address (loader-distinct).
                    class_idx[i] = get_or_insert_class(cid, &|| {
                        p1.class_map
                            .get(&cid)
                            .and_then(|ci| p1.strings.get(&ci.name_id).cloned())
                            .unwrap_or_else(|| format!("unknown@{cid:#x}"))
                    });
                }
            }
        }

        // Free pass1 per-object arrays that are dead after Phase 0b/0c: they
        // are only read to derive  and  above. Releasing
        // them here (~173 MB for a 11 M-object heap) shrinks peak RSS before
        // the edge-scan allocations (inb_flat / fwd_targets).
        p1.class_ids = Vec::new();
        p1.shallow_sizes = Vec::new();
        p1.elem_count = Vec::new();

        // ── Phase 1: Sub-pass 2a — count degrees ────────────────────────
        let mut out_degree: Vec<u32> = vec![0u32; n];
        let mut in_degree: Vec<u32> = vec![0u32; n];

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
                            &class_addrs,
                            &field_plans,
                            &mut size_cache,
                            &mut out_degree,
                            &mut in_degree,
                            &mut shallow,
                            &mut scratch,
                        )?;
                    }
                    tags::HEAP_DUMP_END => break,
                    _ => { r.skip(length)?; }
                }
            }
        }

        // Class objects already map to the java/lang/Class row (JLC_KEY) from Phase 0c.
        let jlc_idx = get_or_insert_class(JLC_KEY, &|| "java/lang/Class".to_string());

        // ── Build class_obj_class_idx ─────────────────────────────────────
        // For each class object, record the histogram row of the class it
        // represents. Under identity keying, that row is keyed by the class
        // object's own address (the same key instances of that class use).
        let mut class_obj_class_idx: Vec<u32> = vec![u32::MAX; n];
        for i in 0..n {
            let addr = p1.id_map.addr_at(i);
            if class_addrs.contains(&addr) {
                let ci = p1.class_map.get(&addr);
                let idx = get_or_insert_class(addr, &|| {
                    ci.and_then(|c| p1.strings.get(&c.name_id).cloned())
                        .unwrap_or_else(|| format!("unknown@{addr:#x}"))
                });
                class_obj_class_idx[i] = idx;
            }
        }
        let _ = jlc_idx;

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
        // ── addSystemClassRootsIfMissing: boot-loader non-array classes not yet roots ─
        let mut synthetic_root_count = 0usize;
        for (&caddr, ci) in &p1.class_map {
            if ci.loader_id != 0 { continue; }
            let is_array = p1.strings.get(&ci.name_id)
                .map(|n| n.starts_with('['))
                .unwrap_or(false);
            if is_array { continue; }
            if let Some(idx) = p1.id_map.index_of(caddr) {
                if !gc_root_set.contains(&(idx as u32)) {
                    gc_root_set.insert(idx as u32);
                    synthetic_root_count += 1;
                }
            }
        }

        // class_map + strings are no longer needed; free before the large edge
        // arrays get allocated in Phase 3/4 to lower peak RSS.
        p1.class_map = std::collections::HashMap::new();
        p1.strings = std::collections::HashMap::new();

        // ── Resolve thread→local synthetic edges ─────────────────────────
        let mut synthetic_edges: Vec<(u32, u32)> = Vec::new();
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

        // ── Phase 3: Build forward-CSR offsets (prefix sum only) ────────
        // fwd_targets is deferred to Phase 3b so it does not coexist with the
        // transient inb_flat scaffolding — lowers peak RSS.
        let mut fwd_offsets: Vec<u32> = Vec::with_capacity(n + 1);
        fwd_offsets.push(0u32);
        for i in 0..n {
            let next = fwd_offsets[i] + out_degree[i];
            fwd_offsets.push(next);
        }
        drop(out_degree); // dead after prefix sum

        // Prefix-sum in_degree counts → cursors; allocate flat inbound array.
        let mut total_inb: u64 = 0;
        for d in in_degree.iter_mut() {
            let cnt = *d as u64;
            *d = total_inb as u32;
            total_inb += cnt;
        }
        let mut inb_flat: Vec<u32> = vec![0u32; total_inb as usize];

        // ── Sub-pass 2b scan: fill INBOUND edges only ────────────────────
        {
            let mut r = HprofReader::open(path)?;
            let mut scratch: Vec<u8> = Vec::with_capacity(4096);
            let mut fwd_t_stub: Vec<u32> = Vec::new();
            let mut fwd_c_stub: Vec<u32> = Vec::new();
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
                            &mut r, id_size, ref_size, length,
                            &p1.id_map, &class_addrs, &field_plans,
                            false, true,
                            &mut fwd_t_stub, &mut fwd_c_stub, &fwd_offsets,
                            &mut inb_flat, &mut in_degree, &mut scratch,
                        )?;
                    }
                    tags::HEAP_DUMP_END => break,
                    _ => { r.skip(length)?; }
                }
            }
        }

        // Synthetic thread→local INBOUND edges.
        for &(src, dst) in &synthetic_edges {
            inb_flat[in_degree[dst as usize] as usize] = src;
            in_degree[dst as usize] += 1;
        }

        // ── Phase 4: Build inbound CSR ────────────────────────────────────
        let mut inb_offsets: Vec<u32> = Vec::with_capacity(n + 1);
        let mut inb_data: Vec<u8> = Vec::new();
        inb_offsets.push(0u32);
        // CSR is contiguous: start[i] = end of node i-1 = in_degree[i-1] after fill.
        let mut start = 0usize;
        for i in 0..n {
            let end = in_degree[i] as usize; // in_degree[i] = end offset after fill
            let slice = &mut inb_flat[start..end];
            // Sort by stripped value (lower 31 bits), ignoring excluded-edge high-bit flag.
            // Delta-encode stripped values only — dominator processes all predecessors equally.
            slice.sort_unstable_by_key(|&v| v & 0x7fff_ffff);
            // Dedup by stripped value (in-place)
            let unique_end = {
                if slice.is_empty() {
                    0
                } else {
                    let mut write = 1usize;
                    for read in 1..slice.len() {
                        if (slice[read] & 0x7fff_ffff) != (slice[write - 1] & 0x7fff_ffff) {
                            slice[write] = slice[read];
                            write += 1;
                        }
                    }
                    write
                }
            };
            // Delta-encode stripped values
            let mut prev: u32 = 0;
            for &v in &slice[..unique_end] {
                let stripped = v & 0x7fff_ffff;
                vbyte::encode(stripped - prev, &mut inb_data);
                prev = stripped;
            }
            inb_offsets.push(inb_data.len() as u32);
            start = end;
        }
        drop(inb_flat);
        drop(in_degree); // inbound CSR done; end-offset cursors no longer needed

        // ── Phase 3b: Build forward CSR (allocated only now, after inb_flat freed) ──
        let total_edges = *fwd_offsets.last().unwrap() as usize;
        let mut fwd_targets: Vec<u32> = vec![u32::MAX; total_edges];
        let mut fwd_cursor: Vec<u32> = fwd_offsets[..n].to_vec();
        {
            let mut r = HprofReader::open(path)?;
            let mut scratch: Vec<u8> = Vec::with_capacity(4096);
            let mut inb_flat_stub: Vec<u32> = Vec::new();
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
                            &mut r, id_size, ref_size, length,
                            &p1.id_map, &class_addrs, &field_plans,
                            true, false,
                            &mut fwd_targets, &mut fwd_cursor, &fwd_offsets,
                            &mut inb_flat_stub, &mut in_degree_stub, &mut scratch,
                        )?;
                    }
                    tags::HEAP_DUMP_END => break,
                    _ => { r.skip(length)?; }
                }
            }
        }
        // Synthetic thread→local FORWARD edges.
        for &(src, dst) in &synthetic_edges {
            let pos = fwd_cursor[src as usize] as usize;
            if pos < fwd_offsets[src as usize + 1] as usize {
                fwd_targets[pos] = dst;
                fwd_cursor[src as usize] += 1;
            }
        }
        drop(fwd_cursor);
        drop(synthetic_edges);

        Ok(Graph {
            n,
            id_size,
            ref_size: ref_size as u8,
            format: p1.format,
            file_size: p1.file_size,
            source_name: std::path::Path::new(path)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string()),
            gc_root_indices,
            shallow,
            class_idx,
            class_names,
            class_obj_class_idx,
            fwd_offsets,
            fwd_targets,
            inb_offsets,
            inb_data,
            synthetic_root_count,
            idom: Vec::new(),
            retained: Vec::new(),
            has_same_class_ancestor: Vec::new(),
        })
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
        class_addrs: &std::collections::HashSet<u64>,
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
                $remaining = $remaining.checked_sub($sz)
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
                    let consumed = Self::count_class_dump_edges(
                        r, id_size, id_map, out_degree, in_degree,
                    )?;
                    checked_sub!(remaining, consumed);
                }
                heap::INSTANCE_DUMP => {
                    let addr = r.id()?;
                    r.skip(4)?;
                    let class_id = r.id()?;
                    let data_len = r.u4()? as u64;
                    if data_len > remaining {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "array too large"));
                    }
                    r.read_bytes_reuse(scratch, data_len as usize)?;
                    checked_sub!(remaining, ids + 4 + ids + 4 + data_len);

                    let src_idx = match id_map.index_of(addr) {
                        Some(i) => i,
                        None => continue,
                    };

                    // Recalculate MAT shallow size for instances (fix #3: reuse size_cache)
                    if !class_addrs.contains(&addr) {
                        let sz = instance_shallow_size(class_id, class_map, ptr_size, ref_size, size_cache);
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
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "array too large"));
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
                        shallow[src_idx] = prim_array_shallow(count, esz as usize, ptr_size, ref_size);
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
        fwd_cursor: &mut Vec<u32>,
        fwd_offsets: &[u32],
        inb_flat: &mut Vec<u32>,
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
                            let pos = fwd_cursor[src] as usize;
                            if pos < fwd_offsets[src + 1] as usize {
                                fwd_targets[pos] = dst as u32;
                                fwd_cursor[src] += 1;
                            }
                        }
                        if do_inb {
                            // Inbound: store src with/without exclusion flag
                            let inb_val = if $excluded {
                                (src as u32) | 0x8000_0000u32
                            } else {
                                src as u32
                            };
                            inb_flat[in_degree[dst] as usize] = inb_val;
                            in_degree[dst] += 1;
                        }
                    }
                }
            };
        }

        macro_rules! checked_sub {
            ($remaining:expr, $sz:expr) => {
                $remaining = $remaining.checked_sub($sz)
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
                        r, id_size, id_map, do_fwd, do_inb,
                        fwd_targets, fwd_cursor, fwd_offsets, inb_flat, in_degree,
                    )?;
                    checked_sub!(remaining, consumed);
                }
                heap::INSTANCE_DUMP => {
                    let addr = r.id()?;
                    r.skip(4)?;
                    let class_id = r.id()?;
                    let data_len = r.u4()? as u64;
                    if data_len > remaining {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "array too large"));
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
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "array too large"));
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
                    let addr = r.id()?;
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
        fwd_cursor: &mut Vec<u32>,
        fwd_offsets: &[u32],
        inb_flat: &mut Vec<u32>,
        in_degree: &mut Vec<u32>,
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
                        if do_fwd {
                            let pos = fwd_cursor[src] as usize;
                            if pos < fwd_offsets[src + 1] as usize {
                                fwd_targets[pos] = dst as u32;
                                fwd_cursor[src] += 1;
                            }
                        }
                        if do_inb {
                            inb_flat[in_degree[dst] as usize] = src as u32;
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
        // Only class objects (e.g. array classes with no static fields) may have
        // shallow 0 — MAT reports 0 for those. All other objects must be > 0.
        for i in 0..g.n {
            if g.shallow[i] == 0 {
                assert!(
                    g.class_obj_class_idx[i] != u32::MAX,
                    "non-class object {i} has shallow 0"
                );
            }
        }
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
