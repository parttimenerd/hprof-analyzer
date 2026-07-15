//! Pass-2 size & field-layout helpers (MAT shallow-size formulas,
//! per-class field plans, compressed-OOP detection, class-name helpers).

use std::collections::HashMap;

use crate::{pass1::ClassInfo, types::HprofType};

// ── Size helpers ───────────────────────────────────────────────────────────

/// Round `n` up to the next multiple of `align`.
pub(crate) fn align_up(n: usize, align: usize) -> usize {
    n.div_ceil(align) * align
}

/// Byte sizes of non-Object (primitive) fields for a class's own fields only.
pub(crate) fn own_prim_bytes(ci: &ClassInfo, _ref_size: usize) -> usize {
    ci.fields
        .iter()
        .filter(|(_, t)| *t != HprofType::Object)
        .map(|(_, t)| t.byte_size())
        .sum()
}

/// Count of Object-typed (reference) fields declared by a class itself.
pub(crate) fn own_obj_count(ci: &ClassInfo) -> usize {
    ci.fields
        .iter()
        .filter(|(_, t)| *t == HprofType::Object)
        .count()
}

/// Recursively compute unaligned instance body size (MAT formula).
pub(crate) fn calculate_size_recursive(
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

/// MAT instance shallow size: recursive body size aligned up to 8 bytes.
pub(crate) fn instance_shallow_size(
    class_addr: u64,
    class_map: &HashMap<u64, ClassInfo>,
    ptr_size: usize,
    ref_size: usize,
    cache: &mut HashMap<u64, usize>,
) -> u32 {
    let inner = calculate_size_recursive(class_addr, class_map, ptr_size, ref_size, cache);
    align_up(inner, 8) as u32
}

/// MAT shallow size of an Object[] array: header + length + `num_elem` refs.
pub(crate) fn obj_array_shallow(num_elem: u64, ptr_size: usize, ref_size: usize) -> u32 {
    align_up(ptr_size + ref_size + 4 + num_elem as usize * ref_size, 8) as u32
}

/// MAT shallow size of a primitive array: aligned header + `num_elem` elements.
pub(crate) fn prim_array_shallow(
    num_elem: u64,
    elem_size: usize,
    ptr_size: usize,
    ref_size: usize,
) -> u32 {
    let header = align_up(ptr_size + ref_size + 4, ref_size);
    align_up(header + num_elem as usize * elem_size, 8) as u32
}

/// MAT shallow size of a class object (java.lang.Class): its static-field bytes
/// only. See the inline note for the no-floor parity detail.
pub(crate) fn class_obj_shallow(ci: &ClassInfo, _ptr_size: usize, ref_size: usize) -> u32 {
    // MAT parity: class-object shallow = alignUp(staticObjFields*refSize + staticPrimBytes, 8).
    // No pointer+ref floor (matClassSize in hprof-analyzer); classes with no statics get 0.
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
pub(crate) fn build_field_plans(
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
pub(crate) fn is_excluded_field(class_name: &str, field_name: &str) -> bool {
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
pub(crate) fn detect_ref_size(id_size: u8, array_addr_counts: &[(u64, u64)]) -> u8 {
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

/// JVM class descriptor for a primitive-array element type code (e.g. 10 -> `[I`).
pub(crate) fn prim_array_class_name(elem_type_code: u8) -> &'static str {
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
pub(crate) fn is_primitive_array_class_name(name: &str) -> bool {
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
pub(crate) fn should_add_system_class_root(
    is_array: bool,
    is_prim_array: bool,
    has_sticky: bool,
) -> bool {
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
pub(crate) fn field_offset(
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
