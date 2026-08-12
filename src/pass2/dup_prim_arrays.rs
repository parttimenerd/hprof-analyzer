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
use std::hash::Hash;

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

/// Accumulates duplicate-primitive-array data inline during the 2a scan,
/// replacing the standalone `compute_dup_prim_arrays` file pass.
/// Call `on_prim_array` for each PRIM_ARRAY_DUMP record, then `finish`.
pub(crate) struct DupPrimCollector {
    hash_map: HashMap<u64, (u32, u64, u8)>, // hash → (count, shallow_bytes, elem_type)
    addr_to_hash: HashMap<u64, u64>,
}

impl DupPrimCollector {
    pub(crate) fn new() -> Self {
        Self {
            hash_map: HashMap::new(),
            addr_to_hash: HashMap::new(),
        }
    }

    /// Ingest one PRIM_ARRAY_DUMP record. `bytes` is the raw element data.
    pub(crate) fn on_prim_array(&mut self, addr: u64, elem_type: u8, _count: u64, bytes: &[u8]) {
        use std::hash::Hasher;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        elem_type.hash(&mut h);
        bytes.hash(&mut h);
        let hv = h.finish();
        let e = self
            .hash_map
            .entry(hv)
            .or_insert((0, bytes.len() as u64, elem_type));
        e.0 = e.0.saturating_add(1);
        self.addr_to_hash.insert(addr, hv);
    }

    /// Finalize: compute dup groups and return `(DupPrimArrays, dup_addrs)`.
    pub(crate) fn finish(self) -> (DupPrimArrays, HashSet<u64>) {
        let Self {
            hash_map,
            addr_to_hash,
        } = self;
        let mut by_type: HashMap<u8, (u64, u64)> = HashMap::new();
        let mut total_wasted: u64 = 0;
        let mut dup_hashes: HashSet<u64> = HashSet::new();
        for (&hv, &(count, shallow, elem_type)) in &hash_map {
            if count <= 1 {
                continue;
            }
            dup_hashes.insert(hv);
            let wasted = (count as u64).saturating_sub(1).saturating_mul(shallow);
            total_wasted = total_wasted.saturating_add(wasted);
            let e = by_type.entry(elem_type).or_insert((0, 0));
            e.0 = e.0.saturating_add(wasted);
            e.1 = e.1.saturating_add(1);
        }
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
        rows.sort_unstable_by(|a, b| {
            b.wasted_bytes
                .cmp(&a.wasted_bytes)
                .then(a.array_class.cmp(&b.array_class))
        });
        rows.truncate(DUP_PRIM_TOP_N);
        (
            DupPrimArrays {
                total_wasted_bytes: total_wasted,
                rows,
                top_array_holders: Vec::new(),
            },
            dup_addrs,
        )
    }
}

/// Build the top-N holder list from counts accumulated during the 2b fill
/// (replaces `compute_dup_array_holders`'s full-file scan).
/// `name_of` resolves a class address to its dotted name.
pub(crate) fn finalize_dup_array_holders(
    class_counter: HashMap<u64, u64>,
    name_of: impl Fn(u64) -> String,
) -> Vec<DupArrayHolder> {
    let mut holders: Vec<DupArrayHolder> = class_counter
        .into_iter()
        .map(|(class_addr, array_refs)| DupArrayHolder {
            class_name: name_of(class_addr),
            array_refs,
        })
        .collect();
    holders.sort_unstable_by(|a, b| {
        b.array_refs
            .cmp(&a.array_refs)
            .then(a.class_name.cmp(&b.class_name))
    });
    holders.truncate(DUP_ARRAY_HOLDER_TOP_N);
    holders
}
