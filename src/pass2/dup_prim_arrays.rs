//! Pass-2 duplicate primitive-array analysis (`--find-duplicates` opt-in).
//!
//! Streams all PRIM_ARRAY_DUMP records once, hashes each array's raw element
//! bytes with a 64-bit hash, and accumulates `hash → (count, shallow, elem_type)`.
//! No element bytes are retained after hashing, so RSS is bounded by the number
//! of distinct arrays (one ~32-byte entry per distinct hash).
//!
//! Returns a [`DupPrimArrays`] struct with total wasted bytes plus a top-N
//! per-element-type breakdown sorted by wasted bytes descending.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{self, ErrorKind};

use crate::{
    reader::HprofReader,
    types::{HprofType, heap, tags},
};

use super::scan::{skip_class_dump, sub_remaining};

/// Top-N element types to report in the breakdown.
const DUP_PRIM_TOP_N: usize = 10;
/// Top-N holder classes to report per dup-array holder ranking.
const DUP_ARRAY_HOLDER_TOP_N: usize = 20;

/// One element-type row in the duplicate-primitive-array breakdown.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct DupPrimArrayRow {
    /// Element type class name, e.g. `"byte[]"`, `"int[]"`, `"long[]"`.
    pub array_class: String,
    /// Number of distinct content groups that have at least one duplicate.
    pub duplicated_groups: u64,
    /// Total wasted bytes for this element type:
    /// Σ over duplicated groups of `(count - 1) * shallow`.
    pub wasted_bytes: u64,
}

/// One holder-class row for the "who holds the most duplicate arrays" ranking.
/// Only populated when `--collections` is also enabled (FieldPlan available).
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct DupArrayHolder {
    /// Fully-qualified class name whose instances hold the most references to
    /// duplicate primitive arrays.
    pub class_name: String,
    /// Number of object-reference fields pointing at duplicate arrays across all
    /// instances of this class.
    pub array_refs: u64,
}

/// Approximate duplicate-primitive-array analysis. Top-N per-type breakdown
/// sorted by wasted bytes descending.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct DupPrimArrays {
    /// Total wasted bytes across all element types.
    pub total_wasted_bytes: u64,
    /// Per-element-type breakdown, sorted by wasted_bytes descending, capped.
    pub rows: Vec<DupPrimArrayRow>,
    /// Top-N classes whose instances hold the most references to duplicate arrays.
    /// Populated only when `--collections` is also on (requires a FieldPlan scan).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_array_holders: Vec<DupArrayHolder>,
}

/// Human-readable name for a HPROF primitive element type code.
fn elem_type_name(code: u8) -> &'static str {
    match code {
        4 => "boolean[]",
        5 => "char[]",
        6 => "float[]",
        7 => "double[]",
        8 => "byte[]",
        9 => "short[]",
        10 => "int[]",
        11 => "long[]",
        _ => "unknown[]",
    }
}

/// Compute approximate duplicate-primitive-array waste. One full-file pass.
/// Also returns the set of duplicate array object IDs (addresses of arrays
/// that appeared in a group of ≥ 2 identical copies) for use by
/// [`compute_dup_array_holders`].
pub(crate) fn compute_dup_prim_arrays(
    path: &str,
    id_size: u8,
) -> io::Result<(DupPrimArrays, HashSet<u64>)> {
    let ids = id_size as u64;
    let mut r = HprofReader::open(path)?;
    let mut scratch: Vec<u8> = Vec::with_capacity(4096);

    // hash → (count, shallow_bytes_per_copy, elem_type_code, first_seen_addr)
    // We track one representative address per group to build the dup-addr set.
    // Because addresses per group can be many, we collect ALL addresses of
    // arrays that land in a duplicated group in a second step below.
    // Instead: map hash → (count, shallow, elem_type, Vec<addr>) — but that is
    // O(N) memory. Compromise: store up to a small cap of addrs, then expand
    // after aggregation using a second scan conceptually.
    // Simpler: track hash → (count, shallow, elem_type); separately track
    // addr → hash for all arrays. Then after the pass, collect all addrs whose
    // hash has count >= 2.
    let mut hash_map: HashMap<u64, (u32, u64, u8)> = HashMap::new();
    let mut addr_to_hash: HashMap<u64, u64> = HashMap::new();

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
                    sub_remaining(&mut remaining, 1)?;
                    match sub_tag {
                        heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED | heap::ROOT_STICKY_CLASS => {
                            r.skip(ids)?;
                            sub_remaining(&mut remaining, ids)?;
                        }
                        heap::ROOT_JNI_GLOBAL => {
                            r.skip(2 * ids)?;
                            sub_remaining(&mut remaining, 2 * ids)?;
                        }
                        heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME | heap::ROOT_THREAD_OBJ => {
                            r.skip(ids + 8)?;
                            sub_remaining(&mut remaining, ids + 8)?;
                        }
                        heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                            r.skip(ids + 4)?;
                            sub_remaining(&mut remaining, ids + 4)?;
                        }
                        heap::HEAP_DUMP_INFO => {
                            r.skip(4 + ids)?;
                            sub_remaining(&mut remaining, 4 + ids)?;
                        }
                        heap::CLASS_DUMP => {
                            let consumed = skip_class_dump(&mut r, id_size)?;
                            sub_remaining(&mut remaining, consumed)?;
                        }
                        heap::INSTANCE_DUMP => {
                            r.skip(ids + 4)?;
                            let _class_id = r.id()?;
                            let data_len = r.u4()? as u64;
                            r.skip(data_len)?;
                            sub_remaining(&mut remaining, ids + 4 + ids + 4 + data_len)?;
                        }
                        heap::OBJ_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            r.skip(ids)?;
                            let byte_len = count.saturating_mul(ids);
                            r.skip(byte_len)?;
                            sub_remaining(&mut remaining, ids + 4 + 4 + ids + byte_len)?;
                        }
                        heap::PRIM_ARRAY_DUMP => {
                            let obj_addr = r.id()?;
                            r.skip(4)?; // serial
                            let count = r.u4()? as u64;
                            let elem_type = r.u1()?;
                            let esz = HprofType::from_code(elem_type)
                                .map(|t| t.byte_size() as u64)
                                .unwrap_or(1);
                            let byte_len = count.saturating_mul(esz);
                            sub_remaining(&mut remaining, ids + 4 + 4 + 1 + byte_len)?;
                            r.read_bytes_reuse(&mut scratch, byte_len as usize)?;
                            // Include elem_type in hash so byte[]{0} ≠ int[]{0}.
                            let mut h = std::collections::hash_map::DefaultHasher::new();
                            elem_type.hash(&mut h);
                            scratch.hash(&mut h);
                            let hv = h.finish();
                            let e = hash_map.entry(hv).or_insert((0, byte_len, elem_type));
                            e.0 = e.0.saturating_add(1);
                            addr_to_hash.insert(obj_addr, hv);
                        }
                        other => {
                            return Err(io::Error::new(
                                ErrorKind::InvalidData,
                                format!(
                                    "unknown heap sub-tag 0x{other:02x} in dup-prim-arrays scan"
                                ),
                            ));
                        }
                    }
                }
            }
            tags::HEAP_DUMP_END => break,
            _ => r.skip(length)?,
        }
    }

    // Aggregate by element type: (wasted_bytes, duplicated_groups).
    let mut by_type: HashMap<u8, (u64, u64)> = HashMap::new();
    let mut total_wasted: u64 = 0;
    // Set of hashes whose groups have count >= 2 (duplicated).
    let mut dup_hashes: HashSet<u64> = HashSet::new();
    for (&hv, &(count, shallow, elem_type)) in &hash_map {
        if count <= 1 {
            continue;
        }
        dup_hashes.insert(hv);
        let wasted = (count as u64 - 1) * shallow;
        total_wasted = total_wasted.saturating_add(wasted);
        let e = by_type.entry(elem_type).or_insert((0, 0));
        e.0 = e.0.saturating_add(wasted);
        e.1 = e.1.saturating_add(1);
    }

    // Build the set of addresses that belong to duplicated groups.
    let dup_addrs: HashSet<u64> = addr_to_hash
        .into_iter()
        .filter(|(_, hv)| dup_hashes.contains(hv))
        .map(|(addr, _)| addr)
        .collect();

    let mut rows: Vec<DupPrimArrayRow> = by_type
        .into_iter()
        .map(
            |(code, (wasted_bytes, duplicated_groups))| DupPrimArrayRow {
                array_class: elem_type_name(code).to_string(),
                duplicated_groups,
                wasted_bytes,
            },
        )
        .collect();
    // Sort by wasted_bytes desc, array_class asc for stability.
    rows.sort_unstable_by(|a, b| {
        b.wasted_bytes
            .cmp(&a.wasted_bytes)
            .then(a.array_class.cmp(&b.array_class))
    });
    rows.truncate(DUP_PRIM_TOP_N);

    Ok((
        DupPrimArrays {
            total_wasted_bytes: total_wasted,
            rows,
            top_array_holders: Vec::new(),
        },
        dup_addrs,
    ))
}

/// Scan all INSTANCE_DUMP records and count how many object-reference fields of
/// each class point at addresses in `dup_addrs` (a set of duplicate primitive-array
/// object IDs). Returns the top-N holder classes sorted by `array_refs` descending.
///
/// Requires a `Pass1` for the class map (FieldPlan building) — only called when
/// `--collections` is also on.
pub(crate) fn compute_dup_array_holders(
    path: &str,
    p1: &crate::pass1::Pass1,
    dup_addrs: &HashSet<u64>,
    id_size: u8,
) -> io::Result<Vec<DupArrayHolder>> {
    use super::strings::scan_all_instances;
    use super::{build_field_plans, read_ref};

    let obj_ref_width = id_size as usize;
    let field_plans = build_field_plans(&p1.class_map, &p1.strings, id_size as usize);
    let mut class_counter: HashMap<u64, u64> = HashMap::new();

    scan_all_instances(path, id_size, |_obj_addr, class_id, blob| {
        let Some(plan) = field_plans.get(&class_id) else {
            return;
        };
        let mut hits: u64 = 0;
        for &(offset, _excluded) in plan {
            let off = offset as usize;
            if off + obj_ref_width > blob.len() {
                continue;
            }
            let r = read_ref(&blob[off..], obj_ref_width);
            if r != 0 && dup_addrs.contains(&r) {
                hits += 1;
            }
        }
        if hits > 0 {
            *class_counter.entry(class_id).or_insert(0) += hits;
        }
    })?;

    let class_map = &p1.class_map;
    let strings = &p1.strings;
    let mut holders: Vec<DupArrayHolder> = class_counter
        .into_iter()
        .map(|(class_addr, array_refs)| {
            let class_name = class_map
                .get(&class_addr)
                .and_then(|ci| strings.get(&ci.name_id))
                .map(|s| s.replace('/', "."))
                .unwrap_or_else(|| format!("0x{class_addr:x}"));
            DupArrayHolder {
                class_name,
                array_refs,
            }
        })
        .collect();
    holders.sort_unstable_by(|a, b| {
        b.array_refs
            .cmp(&a.array_refs)
            .then(a.class_name.cmp(&b.class_name))
    });
    holders.truncate(DUP_ARRAY_HOLDER_TOP_N);
    Ok(holders)
}
